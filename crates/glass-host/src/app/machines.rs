//! Machines: your computers on Tailscale, to open in the viewer or wake.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, Sizable, h_flex};
use gpui_kit::prelude::*;
use gpui_kit::*;

use super::data::Tailnet;
use super::ui::{self, I};
use super::{GlassApp, Store, act, actions};

impl GlassApp {
    pub(super) fn machines(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let store = self.store.read(cx);
        let tailnet = store.tailnet.clone();
        let machines = store.machines.clone();
        let theme = cx.theme().clone();

        let mut rows: Vec<AnyElement> = machines
            .iter()
            .map(|m| {
                let icon = match m.os.as_str() {
                    "macOS" => I::Laptop,
                    "linux" => I::Server,
                    _ => I::Monitor,
                };
                let detail = format!(
                    "{} · {}{}",
                    m.dns,
                    if m.online { "online" } else { "offline" },
                    if m.is_self { " · this computer" } else { "" }
                );
                let url = format!("https://{}/", m.dns);
                let dns = m.dns.clone();
                let control = h_flex()
                    .gap_2()
                    .when(!m.online && m.wakeable, |d| {
                        d.child(Button::new(SharedString::from(format!("wake-{dns}"))).small().ghost().label("Wake").on_click(move |_, window, cx| {
                            let dns = dns.clone();
                            act(window, cx, move || actions::wake(&dns))
                        }))
                    })
                    .when(m.online, |d| {
                        d.child(
                            Button::new(SharedString::from(format!("open-{}", m.dns)))
                                .small()
                                .label("Open")
                                .icon(I::ExternalLink)
                                .on_click(move |_, _, _| actions::open(&url)),
                        )
                    });
                ui::icon_row(icon, if m.online { theme.success } else { theme.muted_foreground }, m.name.clone(), detail, control, cx)
            })
            .collect();
        if rows.is_empty() {
            rows.push(ui::note(
                match tailnet {
                    Tailnet::Checking => "Looking for your machines…",
                    Tailnet::Missing => "Install Tailscale to see your other computers here.",
                    Tailnet::NotRunning | Tailnet::LoggedOut => "Open Tailscale and sign in to see your other computers.",
                    _ => "No other computers on your tailnet yet.",
                },
                cx,
            ));
        }

        ui::page("Machines", "Your computers on Tailscale. Open one in the viewer, or wake one that's asleep.", cx)
            .child(ui::section("On your tailnet", rows, cx))
            .child(
                h_flex().justify_end().child(
                    Button::new("refresh-machines")
                        .small()
                        .ghost()
                        .icon(I::RefreshCw)
                        .label("Refresh")
                        .on_click(|_, _, cx| Store::refresh_tailnet(cx)),
                ),
            )
    }
}
