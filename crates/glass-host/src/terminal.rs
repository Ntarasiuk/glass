//! A shell on the host for the owner, in the viewer (xterm.js), over the control stream.
//!
//! TODO(terminal agent): PTY. Interface is fixed.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum ClientTerm {
    Open { cols: u16, rows: u16 },
    /// Keystrokes, base64.
    Input { data: String },
    Resize { cols: u16, rows: u16 },
    Close,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum ServerTerm {
    /// Output, base64.
    Output { data: String },
    Exit { code: Option<i32> },
}

pub const SUPPORTED: bool = false;

/// One session's shell, if it opened one. Dropping it ends the shell.
#[derive(Default)]
pub struct Session {}

impl Session {
    /// Handles one message; output goes to `out`.
    pub fn handle(
        &mut self,
        msg: ClientTerm,
        out: tokio::sync::mpsc::UnboundedSender<crate::control::ServerMsg>,
    ) -> anyhow::Result<()> {
        let _ = (msg, out);
        anyhow::bail!("the terminal isn't available yet")
    }
}
