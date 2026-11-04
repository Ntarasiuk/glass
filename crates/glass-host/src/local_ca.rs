//! A private certificate authority for HTTPS without `tailscale cert` (e.g. Headscale,
//! which issues none): `glass tls init` makes a CA limited by name constraints to this
//! computer's names and addresses, and a certificate for the host signed by it. Viewers
//! trust the CA once (the /trust page serves it), then the pages load over HTTPS;
//! WebTransport pins a self-signed certificate instead, as Chromium's QUIC doesn't accept
//! CAs installed by the user.
//!
//! Files, in `<config dir>/tls`: ca.crt, ca.key, host.crt, host.key. An existing CA there
//! (made by `glass tls init` or by hand, ECDSA P-256 or Ed25519 with a PKCS#8 key) is
//! reused, so clients that already trust it keep working.

use std::net::IpAddr;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use rcgen::{
    BasicConstraints, CertificateParams, CidrSubnet, DistinguishedName, DnType,
    ExtendedKeyUsagePurpose, GeneralSubtree, IsCa, Issuer, KeyPair, KeyUsagePurpose,
    NameConstraints, SanType,
};
use time::{Duration, OffsetDateTime};

fn sha256(bytes: &[u8]) -> [u8; 32] {
    ring::digest::digest(&ring::digest::SHA256, bytes).as_ref().try_into().unwrap_or([0; 32])
}

/// Browsers cap a server certificate's life at 398 days.
const HOST_DAYS: i64 = 397;
const CA_DAYS: i64 = 3650;
/// Renew the host certificate when it has less than this left.
const RENEW_BEFORE_DAYS: i64 = 30;

pub fn dir() -> PathBuf {
    crate::config_dir().join("tls")
}

pub struct Files {
    pub ca_cert: PathBuf,
    pub ca_key: PathBuf,
    pub cert: PathBuf,
    pub key: PathBuf,
}

pub fn files() -> Files {
    files_in(&dir())
}

fn files_in(d: &std::path::Path) -> Files {
    Files {
        ca_cert: d.join("ca.crt"),
        ca_key: d.join("ca.key"),
        cert: d.join("host.crt"),
        key: d.join("host.key"),
    }
}

/// Names a certificate covers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Names {
    pub dns: Vec<String>,
    pub ips: Vec<IpAddr>,
}

impl Names {
    fn add_dns(&mut self, name: &str) {
        let name = name.trim().trim_end_matches('.').to_ascii_lowercase();
        if !name.is_empty() && !self.dns.contains(&name) {
            self.dns.push(name);
        }
    }

    fn add_ip(&mut self, ip: IpAddr) {
        if !self.ips.contains(&ip) {
            self.ips.push(ip);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.dns.is_empty() && self.ips.is_empty()
    }

    pub fn describe(&self) -> String {
        self.dns
            .iter()
            .cloned()
            .chain(self.ips.iter().map(|ip| ip.to_string()))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// This computer's tailnet names and addresses: the MagicDNS name, the short name and
/// the Tailscale IPs.
pub async fn tailnet_names() -> Result<Names> {
    let out = tokio::process::Command::new(crate::tailscale::cli())
        .args(["status", "--json"])
        .output()
        .await
        .context("running tailscale")?;
    anyhow::ensure!(out.status.success(), "tailscale status: {}", String::from_utf8_lossy(&out.stderr).trim());
    let status: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    let mut names = Names::default();
    if let Some(dns) = status["Self"]["DNSName"].as_str() {
        names.add_dns(dns);
        if let Some(short) = dns.split('.').next() {
            names.add_dns(short);
        }
    }
    if let Some(host) = status["Self"]["HostName"].as_str() {
        names.add_dns(host);
    }
    for ip in crate::tailscale::self_addresses(&status).unwrap_or_default() {
        names.add_ip(ip);
    }
    Ok(names)
}

fn subnet(ip: IpAddr) -> CidrSubnet {
    match ip {
        IpAddr::V4(v4) => CidrSubnet::V4(v4.octets(), [255; 4]),
        IpAddr::V6(v6) => CidrSubnet::V6(v6.octets(), [255; 16]),
    }
}

fn ca_params(names: &Names, now: OffsetDateTime) -> Result<CertificateParams> {
    let mut params = CertificateParams::new(Vec::<String>::new())?;
    let mut dn = DistinguishedName::new();
    let host = names.dns.first().map(String::as_str).unwrap_or("this computer");
    dn.push(DnType::CommonName, format!("Glass {host} local CA"));
    params.distinguished_name = dn;
    params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    // The CA can only vouch for this computer: trusting it can't be used against any
    // other site.
    params.name_constraints = Some(NameConstraints {
        permitted_subtrees: names
            .dns
            .iter()
            .cloned()
            .map(GeneralSubtree::DnsName)
            .chain(names.ips.iter().map(|&ip| GeneralSubtree::IpAddress(subnet(ip))))
            .collect(),
        excluded_subtrees: Vec::new(),
    });
    params.not_before = now - Duration::hours(1);
    params.not_after = now + Duration::days(CA_DAYS);
    Ok(params)
}

fn host_params(names: &Names, now: OffsetDateTime) -> Result<CertificateParams> {
    let mut params = CertificateParams::new(names.dns.clone())?;
    params.subject_alt_names.extend(names.ips.iter().map(|&ip| SanType::IpAddress(ip)));
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, names.dns.first().cloned().unwrap_or_else(|| "glass".into()));
    params.distinguished_name = dn;
    params.is_ca = IsCa::ExplicitNoCa;
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    params.not_before = now - Duration::hours(1);
    params.not_after = now + Duration::days(HOST_DAYS);
    Ok(params)
}

/// Writes `text`, readable by the owner alone when `secret`.
fn write(path: &PathBuf, text: &str, secret: bool) -> Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
    #[cfg(unix)]
    if secret {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    let _ = secret;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// What a certificate on disk says about itself.
pub struct CertInfo {
    pub subject: String,
    pub names: Names,
    pub permitted: Option<Names>,
    pub not_after: OffsetDateTime,
    pub sha256: [u8; 32],
}

pub fn read_cert(path: &PathBuf) -> Result<CertInfo> {
    use x509_parser::extensions::{GeneralName, ParsedExtension};
    let pem_text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let (_, pem) = x509_parser::pem::parse_x509_pem(pem_text.as_bytes())
        .map_err(|e| anyhow::anyhow!("{}: not a PEM certificate ({e})", path.display()))?;
    let cert = pem.parse_x509().map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
    let ip_of = |bytes: &[u8]| -> Option<IpAddr> {
        match bytes.len() {
            4 => Some(IpAddr::from(<[u8; 4]>::try_from(bytes).ok()?)),
            16 => Some(IpAddr::from(<[u8; 16]>::try_from(bytes).ok()?)),
            // Name constraints carry address and mask.
            8 => Some(IpAddr::from(<[u8; 4]>::try_from(&bytes[..4]).ok()?)),
            32 => Some(IpAddr::from(<[u8; 16]>::try_from(&bytes[..16]).ok()?)),
            _ => None,
        }
    };
    let mut names = Names::default();
    let mut permitted = None;
    for ext in cert.extensions() {
        match ext.parsed_extension() {
            ParsedExtension::SubjectAlternativeName(san) => {
                for name in &san.general_names {
                    match name {
                        GeneralName::DNSName(d) => names.add_dns(d),
                        GeneralName::IPAddress(b) => {
                            if let Some(ip) = ip_of(b) {
                                names.add_ip(ip)
                            }
                        }
                        _ => {}
                    }
                }
            }
            ParsedExtension::NameConstraints(nc) => {
                let mut p = Names::default();
                for tree in nc.permitted_subtrees.iter().flatten() {
                    match &tree.base {
                        GeneralName::DNSName(d) => p.add_dns(d),
                        GeneralName::IPAddress(b) => {
                            if let Some(ip) = ip_of(b) {
                                p.add_ip(ip)
                            }
                        }
                        _ => {}
                    }
                }
                permitted = Some(p);
            }
            _ => {}
        }
    }
    let not_after = OffsetDateTime::from_unix_timestamp(cert.validity().not_after.timestamp())
        .unwrap_or(OffsetDateTime::UNIX_EPOCH);
    Ok(CertInfo {
        subject: cert.subject().to_string(),
        names,
        permitted,
        not_after,
        sha256: sha256(&pem.contents),
    })
}

/// Whether a name constraint (DNS) covers `name`: equal, or a subdomain.
fn dns_covered(name: &str, permitted: &[String]) -> bool {
    permitted.iter().any(|p| {
        let p = p.trim_start_matches('.');
        name == p || name.ends_with(&format!(".{p}"))
    })
}

fn issue_host(files: &Files, names: &Names) -> Result<()> {
    let ca_key = KeyPair::from_pem(&std::fs::read_to_string(&files.ca_key)?)
        .with_context(|| format!("{}: the CA's key (PKCS#8 PEM)", files.ca_key.display()))?;
    let ca_pem = std::fs::read_to_string(&files.ca_cert)?;
    let issuer = Issuer::from_ca_cert_pem(&ca_pem, ca_key)
        .with_context(|| format!("{}: the CA's certificate", files.ca_cert.display()))?;
    let key = KeyPair::generate()?;
    let cert = host_params(names, OffsetDateTime::now_utc())?.signed_by(&key, &issuer)?;
    write(&files.key, &key.serialize_pem(), true)?;
    write(&files.cert, &cert.pem(), false)?;
    Ok(())
}

#[derive(Debug)]
pub struct InitReport {
    pub new_ca: bool,
    pub names: Names,
    pub ca_sha256: [u8; 32],
}

/// Makes (or reuses) the CA and issues the host certificate for `names`.
pub fn init(names: &Names, new_ca: bool) -> Result<InitReport> {
    init_in(&dir(), names, new_ca)
}

fn init_in(dir: &std::path::Path, names: &Names, new_ca: bool) -> Result<InitReport> {
    if names.is_empty() {
        bail!("no names for the certificate: is Tailscale up? Or pass --name / --ip");
    }
    let files = files_in(dir);
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    let have_ca = files.ca_cert.is_file() && files.ca_key.is_file();
    let make_ca = new_ca || !have_ca;
    if !make_ca {
        // Reusing the CA: its name constraints must cover the names, or browsers would
        // reject the host certificate.
        let ca = read_cert(&files.ca_cert)?;
        if let Some(p) = &ca.permitted {
            let dns_out: Vec<&String> = names.dns.iter().filter(|d| !dns_covered(d, &p.dns)).collect();
            let ip_out: Vec<&IpAddr> = names.ips.iter().filter(|ip| !p.ips.contains(ip)).collect();
            if !dns_out.is_empty() || !ip_out.is_empty() {
                let out: Vec<String> = dns_out
                    .iter()
                    .map(|s| s.to_string())
                    .chain(ip_out.iter().map(|ip| ip.to_string()))
                    .collect();
                bail!(
                    "the CA in {} only covers {}, not {}. Run `glass tls init --new-ca` for a new CA (viewers then trust it again), or leave those names out",
                    files.ca_cert.display(),
                    p.describe(),
                    out.join(", ")
                );
            }
        }
    }
    if make_ca {
        let key = KeyPair::generate()?;
        let cert = ca_params(names, OffsetDateTime::now_utc())?.self_signed(&key)?;
        write(&files.ca_key, &key.serialize_pem(), true)?;
        write(&files.ca_cert, &cert.pem(), false)?;
    }
    issue_host(&files, names)?;
    Ok(InitReport {
        new_ca: make_ca,
        names: names.clone(),
        ca_sha256: read_cert(&files.ca_cert)?.sha256,
    })
}

/// The host certificate from the local CA, for the server, renewed first when it's
/// close to expiring (and the CA's key is here). None when there's none.
pub fn host_files() -> Option<(PathBuf, PathBuf)> {
    let files = files();
    if !(files.cert.is_file() && files.key.is_file()) {
        return None;
    }
    match read_cert(&files.cert) {
        Ok(info) if info.not_after - OffsetDateTime::now_utc() < Duration::days(RENEW_BEFORE_DAYS) => {
            if files.ca_key.is_file() {
                match issue_host(&files, &info.names) {
                    Ok(()) => tracing::info!("renewed the local CA's certificate for {}", info.names.describe()),
                    Err(e) => tracing::warn!("can't renew {}: {e:#}", files.cert.display()),
                }
            } else {
                tracing::warn!("{} expires {}; no CA key here to renew it", files.cert.display(), info.not_after.date());
            }
        }
        Ok(_) => {}
        Err(e) => {
            tracing::warn!("{e:#}");
            return None;
        }
    }
    tracing::info!("using the local CA's certificate ({})", files.cert.display());
    Some((files.cert, files.key))
}

/// Whether `cert` is the local CA's host certificate.
pub fn is_local(cert: &std::path::Path) -> bool {
    let ours = files().cert;
    match (cert.canonicalize(), ours.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// The CA certificate viewers install (PEM, DER and SHA-256), if there is one.
pub fn ca_cert() -> Option<(String, Vec<u8>, [u8; 32])> {
    let pem_text = std::fs::read_to_string(files().ca_cert).ok()?;
    let (_, pem) = x509_parser::pem::parse_x509_pem(pem_text.as_bytes()).ok()?;
    let sha = sha256(&pem.contents);
    Some((pem_text, pem.contents, sha))
}

pub fn fingerprint(sha: &[u8; 32]) -> String {
    sha.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":")
}

#[derive(clap::Subcommand)]
pub enum TlsCommand {
    /// Make the CA (or reuse the one there) and a certificate for this computer: its
    /// tailnet name, short name and Tailscale addresses, plus any --name / --ip. The CA
    /// can only vouch for those names. Restart Glass afterwards.
    Init {
        /// Another name for the certificate (repeatable).
        #[arg(long = "name", value_name = "DNS")]
        names: Vec<String>,
        /// Another address for the certificate (repeatable).
        #[arg(long = "ip", value_name = "IP")]
        ips: Vec<IpAddr>,
        /// Don't ask Tailscale for this computer's names; only --name / --ip.
        #[arg(long)]
        no_tailnet: bool,
        /// A new CA even if there is one (viewers then have to trust the new one).
        #[arg(long)]
        new_ca: bool,
    },
    /// Show the CA and the host certificate: names, expiry, fingerprint.
    Status,
}

pub async fn cli(action: &TlsCommand) -> Result<()> {
    match action {
        TlsCommand::Init { names: extra, ips, no_tailnet, new_ca } => {
            let mut names = if *no_tailnet {
                Names::default()
            } else {
                match tailnet_names().await {
                    Ok(n) => n,
                    Err(e) => {
                        eprintln!("Tailscale: {e:#} (using only --name / --ip)");
                        Names::default()
                    }
                }
            };
            for n in extra {
                names.add_dns(n);
            }
            for ip in ips {
                names.add_ip(*ip);
            }
            let report = init(&names, *new_ca)?;
            let f = files();
            println!(
                "{} CA: {}\nCertificate for {} ({} days): {}\nCA fingerprint (SHA-256): {}",
                if report.new_ca { "New" } else { "Reused" },
                f.ca_cert.display(),
                report.names.describe(),
                HOST_DAYS,
                f.cert.display(),
                fingerprint(&report.ca_sha256),
            );
            println!(
                "\nGlass uses it when there's no `tailscale cert` (or pass --cert {} --key {}), and renews it {RENEW_BEFORE_DAYS} days before it expires. Restart Glass, then on each viewer open https://<this computer>:<port>/trust (or http://…:<--trust-port>/trust) to install the CA.",
                f.cert.display(),
                f.key.display()
            );
            Ok(())
        }
        TlsCommand::Status => {
            let f = files();
            if !f.ca_cert.is_file() {
                println!("No local CA ({}). Make one with `glass tls init`.", f.ca_cert.display());
                return Ok(());
            }
            let ca = read_cert(&f.ca_cert)?;
            println!("CA: {} ({})", ca.subject, f.ca_cert.display());
            println!("  fingerprint (SHA-256): {}", fingerprint(&ca.sha256));
            println!("  expires: {}", ca.not_after.date());
            if let Some(p) = &ca.permitted {
                println!("  only for: {}", p.describe());
            }
            println!("  key here: {}", if f.ca_key.is_file() { "yes (renews the host certificate)" } else { "no" });
            match read_cert(&f.cert) {
                Ok(host) => {
                    println!("Host certificate: {}", f.cert.display());
                    println!("  for: {}", host.names.describe());
                    println!("  expires: {}", host.not_after.date());
                }
                Err(_) => println!("No host certificate ({}).", f.cert.display()),
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> Names {
        let mut n = Names::default();
        n.add_dns("pc.tail.example.");
        n.add_dns("pc");
        n.add_ip("100.64.0.9".parse().unwrap());
        n
    }

    #[test]
    fn makes_a_constrained_ca_and_a_host_cert_it_signs() {
        let dir = std::env::temp_dir().join(format!("glass-ca-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let report = init_in(&dir, &names(), false).unwrap();
        assert!(report.new_ca);
        let f = files_in(&dir);
        let ca = read_cert(&f.ca_cert).unwrap();
        let p = ca.permitted.expect("name constraints");
        assert_eq!(p.dns, vec!["pc.tail.example", "pc"]);
        assert_eq!(p.ips, vec!["100.64.0.9".parse::<IpAddr>().unwrap()]);
        let host = read_cert(&f.cert).unwrap();
        assert_eq!(host.names, names());
        let days = (host.not_after - OffsetDateTime::now_utc()).whole_days();
        assert!((HOST_DAYS - 1..=HOST_DAYS).contains(&days), "{days}");
        // Again: same CA (viewers keep trusting it), a new host certificate.
        let again = init_in(&dir, &names(), false).unwrap();
        assert!(!again.new_ca);
        assert_eq!(again.ca_sha256, report.ca_sha256);
        // A name the CA doesn't cover is refused rather than issued.
        let mut more = names();
        more.add_dns("other.example");
        assert!(init_in(&dir, &more, false).unwrap_err().to_string().contains("other.example"));
        // The host certificate chains to the CA (rustls with the CA as the only root).
        let roots = {
            let mut store = rustls::RootCertStore::empty();
            let (_, pem) = x509_parser::pem::parse_x509_pem(std::fs::read(&f.ca_cert).unwrap().as_slice()).unwrap();
            store.add(pem.contents.into()).unwrap();
            std::sync::Arc::new(store)
        };
        let _ = rustls::crypto::ring::default_provider().install_default();
        use rustls::client::danger::ServerCertVerifier;
        let verifier = rustls::client::WebPkiServerVerifier::builder(roots).build().unwrap();
        let (_, leaf) = x509_parser::pem::parse_x509_pem(std::fs::read(&f.cert).unwrap().as_slice()).unwrap();
        let leaf: rustls::pki_types::CertificateDer = leaf.contents.into();
        for name in ["pc.tail.example", "pc", "100.64.0.9"] {
            let server = rustls::pki_types::ServerName::try_from(name).unwrap();
            verifier
                .verify_server_cert(&leaf, &[], &server, &[], rustls::pki_types::UnixTime::now())
                .unwrap_or_else(|e| panic!("{name}: {e}"));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
