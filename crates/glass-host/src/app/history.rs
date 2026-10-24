//! History: who connected, when, how, and for how long.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, Sizable, h_flex};
use gpui_kit::prelude::*;
use gpui_kit::*;

use super::ui::{self, I};
use super::{GlassApp, actions, data};
use crate::connlog::Entry;

/// Shown at most; the file keeps more.
const SHOWN: usize = 100;

pub fn entry_row(e: &Entry, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let refused = e.how == "refused";
    let (icon, tint) = match e.how.as_str() {
        "refused" => (I::Ban, theme.danger),
        "guest" => (I::Link, theme.primary),
        "token" => (I::KeyRound, theme.primary),
        _ => (I::Network, theme.success),
    };
    let how = match e.how.as_str() {
        "tailnet" => "Your tailnet",
        "token" => "With the token",
        "guest" => "With a link",
        "refused" => "Turned away",
        other => other,
    };
    let mut detail = vec![how.to_string()];
    if e.remote {
        detail.push("from anywhere".into());
    }
    if !e.detail.is_empty() {
        detail.push(e.detail.clone());
    }
    if !refused && e.secs > 0 {
        detail.push(crate::access::describe(std::time::Duration::from_secs(e.secs)));
    }
    ui::icon_row(
        icon,
        tint,
        if e.device.is_empty() { "A browser".to_string() } else { e.device.clone() },
        detail.join(" · "),
        div().text_xs().text_color(theme.muted_foreground).child(data::when(e.start)),
        cx,
    )
}

impl GlassApp {
    pub(super) fn history(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let log = self.store.read(cx).snap.log.clone();
        let mut rows: Vec<AnyElement> = log.iter().take(SHOWN).map(|e| entry_row(e, cx)).collect();
        if rows.is_empty() {
            rows.push(ui::note("No connections yet.", cx));
        }
        ui::page("History", "Every session and every refused attempt, newest first.", cx)
            .child(ui::group(rows, cx))
            .child(
                h_flex().justify_end().child(
                    Button::new("open-log").small().ghost().icon(I::FileText).label("Open the full log").on_click(|_, _, _| {
                        actions::open(&crate::connlog::path().display().to_string())
                    }),
                ),
            )
    }
}
