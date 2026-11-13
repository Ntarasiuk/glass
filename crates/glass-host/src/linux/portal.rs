//! Screen-cast session via xdg-desktop-portal. On Hyprland this shows the share picker
//! once; the restore token we save lets later starts skip it.

use std::os::fd::OwnedFd;
use std::sync::mpsc;

use anyhow::{Context, Result};
use ashpd::desktop::PersistMode;
use ashpd::desktop::screencast::{CursorMode, Screencast, SelectSourcesOptions, SourceType};

pub struct PortalStream {
    pub node_id: u32,
    /// Logical position and size in the compositor layout, when the portal reports them.
    pub position: Option<(i32, i32)>,
    pub size: Option<(i32, i32)>,
}

pub struct Cast {
    pub fd: OwnedFd,
    pub streams: Vec<PortalStream>,
    /// Dropping it ends the portal session.
    pub session: tokio::sync::oneshot::Sender<()>,
}

/// The person at the computer didn't approve the share: not to be asked again unprompted.
#[derive(Debug)]
pub struct NotApproved;

impl std::fmt::Display for NotApproved {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "screen sharing was not approved")
    }
}

impl std::error::Error for NotApproved {}

fn token_path() -> std::path::PathBuf {
    crate::config_dir().join("portal-restore-token")
}

/// Starts a session, kept open until `Cast::session` is dropped. Blocks until the user
/// approves (or cancels) the share dialog.
pub fn start() -> Result<Cast> {
    let (tx, rx) = mpsc::channel();
    portal_thread().send(tx).map_err(|_| anyhow::anyhow!("the portal thread stopped"))?;
    rx.recv().context("portal thread exited")?
}

/// One thread and runtime for every session. ashpd keeps a single D-Bus connection for the
/// process, and zbus drives it with tasks on the runtime that made it: a runtime per session
/// would leave every later portal call (and the file chooser) waiting on one that's gone.
fn portal_thread() -> &'static tokio::sync::mpsc::UnboundedSender<mpsc::Sender<Result<Cast>>> {
    static THREAD: std::sync::OnceLock<tokio::sync::mpsc::UnboundedSender<mpsc::Sender<Result<Cast>>>> =
        std::sync::OnceLock::new();
    THREAD.get_or_init(|| {
        let (requests, mut incoming) = tokio::sync::mpsc::unbounded_channel::<mpsc::Sender<Result<Cast>>>();
        let spawned = std::thread::Builder::new().name("glass-portal".into()).spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(r) => r,
                Err(e) => return tracing::warn!("portal runtime: {e}"),
            };
            runtime.block_on(async move {
                while let Some(reply) = incoming.recv().await {
                    tokio::spawn(session(reply));
                }
            });
        });
        if let Err(e) = spawned {
            tracing::warn!("portal thread: {e}");
        }
        requests
    })
}

/// Opens a session, replies with it, and holds it until `Cast::session` is dropped.
async fn session(reply: mpsc::Sender<Result<Cast>>) {
    let opened = match answered(Screencast::new()).await {
        Ok(proxy) => open(&proxy).await,
        Err(e) => Err(e.context("screen-cast portal unavailable")),
    };
    match opened {
        Ok(((fd, streams), session)) => {
            let (hold, ended) = tokio::sync::oneshot::channel();
            if reply.send(Ok(Cast { fd, streams, session: hold })).is_ok() {
                // Dropping the session would end the cast; hold it until told.
                let _ = ended.await;
            }
            // Dropping a Session doesn't close it.
            let _ = answered(session.close()).await;
        }
        Err(e) => {
            let _ = reply.send(Err(e));
        }
    }
}

/// How long a portal call that no one has to answer may take. One sent while the portal is
/// (re)starting can go unanswered, and the share, and everyone waiting on it, would be stuck.
const ANSWER: std::time::Duration = std::time::Duration::from_secs(15);

async fn answered<T, E: Into<anyhow::Error>>(call: impl Future<Output = Result<T, E>>) -> Result<T> {
    match tokio::time::timeout(ANSWER, call).await {
        Ok(r) => r.map_err(Into::into),
        Err(_) => anyhow::bail!("the screen-cast portal didn't answer in {} s", ANSWER.as_secs()),
    }
}

async fn open(proxy: &Screencast) -> Result<((OwnedFd, Vec<PortalStream>), ashpd::desktop::Session<Screencast>)> {
    let session = answered(proxy.create_session(Default::default())).await?;
    match share(proxy, &session).await {
        Ok(cast) => Ok((cast, session)),
        Err(e) => {
            let _ = answered(session.close()).await;
            Err(e)
        }
    }
}

/// Selecting sources and starting can wait on the person at the computer (the share dialog),
/// so they aren't timed.
async fn share(proxy: &Screencast, session: &ashpd::desktop::Session<Screencast>) -> Result<(OwnedFd, Vec<PortalStream>)> {
    let token = std::fs::read_to_string(token_path()).ok();
    proxy
        .select_sources(
            session,
            SelectSourcesOptions::default()
                .set_cursor_mode(CursorMode::Embedded)
                .set_sources(ashpd::enumflags2::BitFlags::from_flag(SourceType::Monitor))
                .set_multiple(true)
                .set_restore_token(token.as_deref().map(str::trim))
                .set_persist_mode(PersistMode::ExplicitlyRevoked),
        )
        .await?;
    let response = proxy
        .start(session, None, Default::default())
        .await?
        .response()
        .map_err(|_| anyhow::Error::new(NotApproved))?;
    if let Some(token) = response.restore_token() {
        let _ = std::fs::create_dir_all(crate::config_dir());
        let _ = std::fs::write(token_path(), token);
    }
    let streams = response
        .streams()
        .iter()
        .map(|s| PortalStream {
            node_id: s.pipe_wire_node_id(),
            position: s.position(),
            size: s.size(),
        })
        .collect::<Vec<_>>();
    anyhow::ensure!(!streams.is_empty(), "no screens were shared");
    let fd = answered(proxy.open_pipe_wire_remote(session, Default::default())).await?;
    Ok((fd, streams))
}
