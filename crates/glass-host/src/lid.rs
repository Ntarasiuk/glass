//! Staying awake with the lid closed (macOS), so a MacBook closed on a desk can still be
//! unlocked and used from a viewer. Closing the lid puts a Mac to sleep unless a real
//! display is attached (a virtual one doesn't count), and only `pmset disablesleep`, as
//! root, stops that. Turning `lid-awake` on installs a sudoers rule that allows exactly
//! that toggle, once, with the admin password. The host then keeps sleep off only while
//! the Mac is on power and turns it back on on battery, so a Mac closed in a bag sleeps.

#[cfg(target_os = "macos")]
use std::process::Command;

#[cfg(target_os = "macos")]
const RULE: &str = "/etc/sudoers.d/glass-lid";
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const PMSET: &str = "/usr/bin/pmset";

/// The sudoers rule for `user`: the two exact commands, nothing else.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn rule(user: &str) -> String {
    format!("# Written by Glass (glass config lid-awake on)\n{user} ALL=(root) NOPASSWD: {PMSET} -a disablesleep 0, {PMSET} -a disablesleep 1\n")
}

/// A user name safe to put in a sudoers rule.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn plain_name(user: &str) -> bool {
    !user.is_empty() && user.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Lets Glass turn sleep off and on without a password: installs the rule, asking for the
/// admin password (in this terminal, or the system's dialog) unless it's there already.
pub fn allow() -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        if allowed() {
            return Ok(());
        }
        let user = std::env::var("USER").unwrap_or_default();
        anyhow::ensure!(plain_name(&user), "can't tell which user to allow");
        let tmp = std::env::temp_dir().join(format!("glass-lid-{}", std::process::id()));
        std::fs::write(&tmp, rule(&user))?;
        let checked = Command::new("/usr/sbin/visudo").arg("-cf").arg(&tmp).output()?.status.success();
        if !checked {
            let _ = std::fs::remove_file(&tmp);
            anyhow::bail!("the sudoers rule didn't pass visudo");
        }
        use std::io::IsTerminal;
        let ask = if std::io::stdin().is_terminal() { crate::mic::Ask::Terminal } else { crate::mic::Ask::Dialog };
        let installed = crate::mic::as_admin(
            &format!("/usr/bin/install -m 0440 -o root -g wheel '{}' {RULE}", tmp.display()),
            ask,
            "let it stay awake with the lid closed",
        );
        let _ = std::fs::remove_file(&tmp);
        installed?;
        anyhow::ensure!(allowed(), "the rule was installed, but sudo still asks for a password");
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    anyhow::bail!("lid-awake is for Mac laptops")
}

/// Sudo lets this user toggle sleep without a password.
#[cfg(target_os = "macos")]
fn allowed() -> bool {
    Command::new("/usr/bin/sudo")
        .args(["-n", "-l", PMSET, "-a", "disablesleep", "1"])
        .output()
        .is_ok_and(|o| o.status.success())
}

#[cfg(target_os = "macos")]
fn set_sleep_disabled(on: bool) -> bool {
    Command::new("/usr/bin/sudo")
        .args(["-n", PMSET, "-a", "disablesleep", if on { "1" } else { "0" }])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Sleep is off, from `pmset -g` (it lists `SleepDisabled` only while it's on).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn parse_sleep_disabled(pmset_g: &str) -> bool {
    pmset_g.lines().any(|l| {
        let mut words = l.split_whitespace();
        words.next() == Some("SleepDisabled") && words.next() == Some("1")
    })
}

/// The Mac is on power, from `pmset -g batt` (a Mac without a battery always is).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn parse_on_power(pmset_batt: &str) -> bool {
    !pmset_batt.contains("'Battery Power'")
}

#[cfg(target_os = "macos")]
fn pmset(args: &[&str]) -> String {
    Command::new(PMSET).args(args).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
}

/// Glass turned sleep off (so it's Glass's to turn back on, not someone else's setting).
#[cfg(target_os = "macos")]
fn marker() -> std::path::PathBuf {
    crate::config_dir().join("lid-awake-applied")
}

/// Keeps sleep off while `lid-awake` is on and the Mac is on power, and on again
/// otherwise. Checks every few seconds, for as long as the host runs.
pub fn watch() {
    #[cfg(target_os = "macos")]
    std::thread::spawn(|| {
        let mut warned = false;
        loop {
            let want = crate::settings::load().lid_awake && parse_on_power(&pmset(&["-g", "batt"]));
            let off = parse_sleep_disabled(&pmset(&["-g"]));
            let ours = marker().exists();
            if want && !off {
                if set_sleep_disabled(true) {
                    let _ = std::fs::write(marker(), b"");
                    warned = false;
                    tracing::info!("lid-awake: on power, so a closed lid won't sleep");
                } else if !warned {
                    warned = true;
                    tracing::warn!("lid-awake: can't turn sleep off; run `glass config lid-awake on` again");
                }
            } else if !want && off && ours {
                if set_sleep_disabled(false) {
                    let _ = std::fs::remove_file(marker());
                    tracing::info!("lid-awake: sleep allowed again");
                }
            } else if !want && ours && !off {
                let _ = std::fs::remove_file(marker());
            }
            std::thread::sleep(std::time::Duration::from_secs(5));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rule_allows_only_the_two_toggles() {
        assert_eq!(
            rule("nathan").lines().nth(1),
            Some("nathan ALL=(root) NOPASSWD: /usr/bin/pmset -a disablesleep 0, /usr/bin/pmset -a disablesleep 1")
        );
        assert!(plain_name("first.last-2"));
        assert!(!plain_name("nathan ALL=(ALL) ALL"));
        assert!(!plain_name(""));
    }

    #[test]
    fn reads_pmset() {
        assert!(parse_sleep_disabled(" sleep 1\n SleepDisabled\t\t1\n"));
        assert!(!parse_sleep_disabled(" SleepDisabled\t\t0\n"));
        assert!(!parse_sleep_disabled(" sleep 1\n"));
        assert!(parse_on_power("Now drawing from 'AC Power'\n -InternalBattery-0 80%"));
        assert!(!parse_on_power("Now drawing from 'Battery Power'\n -InternalBattery-0 80%"));
    }
}
