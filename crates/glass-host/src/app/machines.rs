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
                        #[cfg(any(target_os = "macos", windows))]
                        let d = {
                            let target = crate::viewer::Target { name: m.name.clone(), dns: m.dns.clone(), is_self: m.is_self, token: None, link: None, remote: None };
                            d.child(
                                Button::new(SharedString::from(format!("connect-{}", m.dns)))
                                    .small()
                                    .primary()
                                    .label("Connect")
                                    .icon(I::Monitor)
                                    .on_click(cx.listener(move |this, _, window, cx| this.connect(target.clone(), window, cx))),
                            )
                        };
                        d.child(
                            Button::new(SharedString::from(format!("open-{}", m.dns)))
                                .small()
                                .ghost()
                                .label("Browser")
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

        ui::page("Machines", "Your computers on Tailscale. Connect to one right here, open it in the browser, or wake one that's asleep.", cx)
            .child(ui::section("On your tailnet", rows, cx))
            .children(self.link_section(cx))
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

impl GlassApp {
    /// Somebody else's computer, from a link they sent (or any Glass address).
    #[cfg(any(target_os = "macos", windows))]
    fn link_section(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        use gpui_kit::component::input::Input;
        let control = h_flex()
            .gap_2()
            .w(px(420.))
            .child(div().flex_1().child(Input::new(&self.open_link).small()))
            .child(Button::new("open-link").small().primary().label("Connect").on_click(cx.listener(|this, _, window, cx| {
                let url = this.open_link.read(cx).value().to_string();
                match crate::viewer::Target::from_url(&url) {
                    Some(target) => this.connect(target, window, cx),
                    None => super::toast(window, cx, Err(anyhow::anyhow!("That doesn't look like a Glass link."))),
                }
            })));
        Some(ui::section("Open a link", vec![ui::row("A link someone sent you", "Or any computer's Glass address.", control, cx)], cx).into_any_element())
    }

    #[cfg(not(any(target_os = "macos", windows)))]
    fn link_section(&mut self, _: &mut Context<Self>) -> Option<AnyElement> {
        None
    }
}
