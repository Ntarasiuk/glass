//! Machines: your computers on Tailscale, on this network and on your glassdesk.dev
//! account, to open in the viewer or wake.

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
        let nearby = store.nearby_only();
        let account = store.account.is_some().then(|| store.account_others());
        let on_account = store.on_account();
        let signing_in = store.signing_in.is_some();
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
                        // Not this computer: its own screen inside itself, and its keys typed back into it.
                        let d = if m.is_self {
                            d
                        } else {
                            let target = crate::viewer::Target { name: m.name.clone(), dns: m.dns.clone(), is_self: m.is_self, token: None, link: None, remote: None, at: None };
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
        let mut nearby_rows = self.nearby_rows(&nearby, cx);
        // Always there, so an empty network says why rather than showing nothing.
        if nearby_rows.is_empty() {
            nearby_rows.push(ui::note(
                if cfg!(any(target_os = "macos", windows)) {
                    "No other computers running Glass found on this network. They show up here by themselves once they run Glass 0.15.2 or newer on the same Wi-Fi or wired network; a VPN on either computer can hide them."
                } else {
                    "Computers on this network are listed in the Glass app on a Mac or Windows PC, or with: glass nearby"
                },
                cx,
            ));
        }
        let account_rows = match &account {
            Some(list) => {
                let mut rows = self.account_rows(list, cx);
                if rows.is_empty() {
                    rows.push(ui::note(
                        match on_account {
                            Some(true) => "Only this computer is on your account so far. Add another from its Glass window, or with: glass account add",
                            _ => "No computers on your account yet. Add one from its Glass window, or with: glass account add",
                        },
                        cx,
                    ));
                }
                rows.push(ui::row(
                    "Signed in to your account",
                    "This app lists your account's computers. Opening one still takes that computer's own sign-in, the first time.",
                    Button::new("account-out-m").small().ghost().label("Sign out").on_click(|_, window, cx| Store::account_sign_out(window, cx)),
                    cx,
                ));
                rows
            }
            None => vec![ui::icon_row(
                I::KeyRound,
                theme.primary,
                "Sign in to your account",
                if signing_in { "Say yes in your browser, where your passkey is. Waiting…" } else { "Your passkey at glassdesk.dev lists your computers here, to open from anywhere." },
                Button::new("account-in-m")
                    .small()
                    .primary()
                    .label(if signing_in { "Open the page again" } else { "Sign in with a passkey" })
                    .on_click(|_, window, cx| Store::account_sign_in(window, cx)),
                cx,
            )],
        };

        ui::page("Machines", "Your computers on Tailscale, on this network and on your account. Connect to one right here, open it in the browser, or wake one that's asleep.", cx)
            .child(ui::section("On this network", nearby_rows, cx))
            .child(ui::section("On your account", account_rows, cx))
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
    /// Glass computers heard on this network. Connecting asks the person there, the first time.
    #[cfg(any(target_os = "macos", windows))]
    pub(super) fn nearby_rows(&mut self, nearby: &[crate::nearby::Found], cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme().clone();
        nearby
            .iter()
            .map(|f| {
                let icon = match f.os.as_str() {
                    "macos" => I::Laptop,
                    "linux" => I::Server,
                    _ => I::Monitor,
                };
                let known = crate::viewer::prefs::pass(&f.url).is_some();
                let detail = format!("{} · {}", f.at, if known { "lets this computer in" } else { "someone there allows it the first time" });
                let target = crate::viewer::Target { name: f.name.clone(), dns: f.url.clone(), is_self: false, token: None, link: None, remote: None, at: Some(f.at) };
                let control = Button::new(SharedString::from(format!("connect-nearby-{}", f.id)))
                    .small()
                    .primary()
                    .label("Connect")
                    .icon(I::Monitor)
                    .on_click(cx.listener(move |this, _, window, cx| this.connect(target.clone(), window, cx)));
                ui::icon_row(icon, theme.success, f.name.clone(), detail, control, cx)
            })
            .collect()
    }

    #[cfg(not(any(target_os = "macos", windows)))]
    pub(super) fn nearby_rows(&mut self, _: &[crate::nearby::Found], _: &mut Context<Self>) -> Vec<AnyElement> {
        Vec::new()
    }

    /// The computers on your glassdesk.dev account, reached from anywhere.
    pub(super) fn account_rows(&mut self, list: &[crate::account::Computer], cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme().clone();
        let base = crate::remote::base().trim_end_matches('/').to_string();
        list.iter()
            .map(|c| {
                let url = format!("{base}/app/#r={}", c.key);
                let detail = if c.online { "online · from anywhere" } else { "offline" };
                let control = h_flex()
                    .gap_2()
                    .when(c.online, |d| {
                        #[cfg(any(target_os = "macos", windows))]
                        let d = match crate::viewer::Target::from_url(&url) {
                            Some(mut target) => {
                                target.name = c.name.clone();
                                d.child(
                                    Button::new(SharedString::from(format!("connect-account-{}", c.id)))
                                        .small()
                                        .primary()
                                        .label("Connect")
                                        .icon(I::Monitor)
                                        .on_click(cx.listener(move |this, _, window, cx| this.connect(target.clone(), window, cx))),
                                )
                            }
                            None => d,
                        };
                        d.child(Button::new(SharedString::from(format!("open-account-{}", c.id))).small().ghost().label("Browser").icon(I::ExternalLink).on_click(move |_, _, _| actions::open(&url)))
                    });
                ui::icon_row(I::Monitor, if c.online { theme.success } else { theme.muted_foreground }, c.name.clone(), detail, control, cx)
            })
            .collect()
    }

    /// Somebody else's computer, from a link they sent (or any Glass address).
    #[cfg(any(target_os = "macos", windows))]
    fn link_section(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        use gpui_kit::component::input::Input;
        let control = h_flex()
            .gap_2()
            .w(super::text_size::pt(420.))
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
