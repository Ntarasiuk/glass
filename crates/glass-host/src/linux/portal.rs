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
    /// Dropping it ends the portal session (and its thread).
    pub close: tokio::sync::oneshot::Sender<()>,
}

fn token_path() -> std::path::PathBuf {
    crate::config_dir().join("portal-restore-token")
}

/// Starts a session, kept open until `Cast::close` is dropped. Blocks until the user
/// approves (or cancels) the share dialog; with a restore token saved from an earlier
/// approval there's no dialog.
pub fn start() -> Result<Cast> {
    start_within(None)
}

/// Like `start`, giving up after `limit` (for reopening with the restore token, where no
/// dialog waits on anyone: a request sent while the portal is still starting can go
/// unanswered).
pub fn start_within(limit: Option<std::time::Duration>) -> Result<Cast> {
    let (tx, rx) = mpsc::channel();
    portal_thread()
        .send((limit, tx))
        .map_err(|_| anyhow::anyhow!("the portal thread stopped"))?;
    rx.recv().context("portal thread exited")?
}

type Request = (Option<std::time::Duration>, mpsc::Sender<Result<Cast>>);

/// One thread and runtime for every session: ashpd keeps its D-Bus connection for the
/// process, driven by the runtime that made it, so that runtime has to outlive them all.
fn portal_thread() -> &'static tokio::sync::mpsc::UnboundedSender<Request> {
    static THREAD: std::sync::OnceLock<tokio::sync::mpsc::UnboundedSender<Request>> =
        std::sync::OnceLock::new();
    THREAD.get_or_init(|| {
        let (requests, mut incoming) = tokio::sync::mpsc::unbounded_channel::<Request>();
        let spawned = std::thread::Builder::new().name("glass-portal".into()).spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!("portal runtime: {e}");
                    return;
                }
            };
            runtime.block_on(async move {
                while let Some((limit, reply)) = incoming.recv().await {
                    tokio::spawn(session(limit, reply));
                }
            });
        });
        if let Err(e) = spawned {
            tracing::warn!("portal thread: {e}");
        }
        requests
    })
}

/// Opens a session, replies with it, and holds it until the capture lets go.
async fn session(limit: Option<std::time::Duration>, reply: mpsc::Sender<Result<Cast>>) {
    let opened = async {
        let proxy = Screencast::new()
            .await
            .map_err(|e| anyhow::Error::from(e).context("screen-cast portal unavailable"))?;
        let (cast, session) = open(&proxy).await?;
        anyhow::Ok((cast, session))
    };
    let opened = match limit {
        Some(limit) => match tokio::time::timeout(limit, opened).await {
            Ok(r) => r,
            Err(_) => Err(anyhow::anyhow!("the portal didn't answer within {}s", limit.as_secs())),
        },
        None => opened.await,
    };
    match opened {
        Ok(((fd, streams), session)) => {
            let (close, closed) = tokio::sync::oneshot::channel::<()>();
            if reply.send(Ok(Cast { fd, streams, close })).is_err() {
                return;
            }
            // Dropping the session ends the cast: hold it until the capture lets go (it
            // reopens one when the portal loses the stream).
            let _ = closed.await;
            let _ = session.close().await;
        }
        Err(e) => {
            let _ = reply.send(Err(e));
        }
    }
}

async fn open(
    proxy: &Screencast,
) -> Result<((OwnedFd, Vec<PortalStream>), ashpd::desktop::Session<Screencast>)> {
    let session = proxy.create_session(Default::default()).await?;
    let token = std::fs::read_to_string(token_path()).ok();
    proxy
        .select_sources(
            &session,
            SelectSourcesOptions::default()
                .set_cursor_mode(CursorMode::Embedded)
                .set_sources(ashpd::enumflags2::BitFlags::from_flag(SourceType::Monitor))
                .set_multiple(true)
                .set_restore_token(token.as_deref().map(str::trim))
                .set_persist_mode(PersistMode::ExplicitlyRevoked),
        )
        .await?;
    let response = proxy
        .start(&session, None, Default::default())
        .await?
        .response()
        .context("screen sharing was not approved")?;
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
    let fd = proxy
        .open_pipe_wire_remote(&session, Default::default())
        .await?;
    Ok(((fd, streams), session))
}
