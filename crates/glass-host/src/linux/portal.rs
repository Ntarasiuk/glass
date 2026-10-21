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
}

fn token_path() -> std::path::PathBuf {
    crate::config_dir().join("portal-restore-token")
}

/// Starts a session on a dedicated thread that keeps it open for the life of the process.
/// Blocks until the user approves (or cancels) the share dialog.
pub fn start() -> Result<Cast> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new().name("glass-portal".into()).spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
            Ok(r) => r,
            Err(e) => return drop(tx.send(Err(anyhow::Error::from(e)))),
        };
        runtime.block_on(async move {
            let proxy = match Screencast::new().await {
                Ok(p) => p,
                Err(e) => return drop(tx.send(Err(anyhow::Error::from(e).context("screen-cast portal unavailable")))),
            };
            match open(&proxy).await {
                Ok((cast, session)) => {
                    let _ = tx.send(Ok(cast));
                    // Dropping the session would end the cast; hold it forever.
                    let _session = session;
                    std::future::pending::<()>().await;
                }
                Err(e) => {
                    let _ = tx.send(Err(e));
                }
            }
        });
    })?;
    rx.recv().context("portal thread exited")?
}

async fn open(proxy: &Screencast) -> Result<(Cast, ashpd::desktop::Session<Screencast>)> {
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
        .map(|s| PortalStream { node_id: s.pipe_wire_node_id(), position: s.position(), size: s.size() })
        .collect::<Vec<_>>();
    anyhow::ensure!(!streams.is_empty(), "no screens were shared");
    let fd = proxy.open_pipe_wire_remote(&session, Default::default()).await?;
    Ok((Cast { fd, streams }, session))
}
