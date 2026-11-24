//! Machines: your computers on this network, the ones opened before (wherever this one is
//! now: over a VPN into their network, say), on your glassdesk.dev account and on
//! Tailscale, to open in the viewer or wake.

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
        let saved = store.saved_elsewhere();
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
                let dns = m.dns.clone();
                // This computer's own address is known; another's is asked for.
                let own = m.is_self.then(|| store.snap.url()).flatten();
                let open_dns = m.dns.clone();
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
                            let target = crate::viewer::Target { name: m.name.clone(), dns: m.dns.clone(), is_self: m.is_self, token: None, link: None, remote: None, typed: None, at: None };
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
                                .on_click(move |_, window, cx| match &own {
                                    Some(url) => actions::open(url),
                                    None => {
                                        let dns = open_dns.clone();
                                        act(window, cx, move || actions::open_machine(&dns, None))
                                    }
                                }),
                        )
                    });
                ui::icon_row(icon, if m.online { theme.success } else { theme.muted_foreground }, m.name.clone(), detail, control, cx)
            })
            .collect();
        if rows.is_empty() {
            rows.push(ui::note(
                match tailnet {
                    Tailnet::Checking => "Looking for your machines…",
                    Tailnet::Missing => "Tailscale isn't installed. With it, your computers there are listed here; without it, the lists above have them.",
                    Tailnet::NotRunning | Tailnet::LoggedOut => "Open Tailscale and sign in to see your computers there.",
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
                    "No other computers running Glass found on this network. They show up here by themselves once they run Glass 0.15.2 or newer on the same Wi-Fi or wired network. Over a VPN nothing shows up by itself: the ones you've opened before are listed below, and any other opens by its address."
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

        ui::page("Machines", "Your computers on this network, over your VPN, on your account and on Tailscale. Connect to one right here, open it in the browser, or wake one that's asleep.", cx)
            .child(ui::section("On this network", nearby_rows, cx))
            .children(self.saved_section(&saved, cx))
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
                        .on_click(|_, _, cx| {
                            Store::refresh_tailnet(cx);
                            Store::refresh_saved(cx);
                        }),
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
                let target = crate::viewer::Target { name: f.name.clone(), dns: f.url.clone(), is_self: false, token: None, link: None, remote: None, typed: None, at: Some(f.at) };
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

    /// The computers opened before that aren't heard on this network: reached at the address
    /// they had, when that answers from here (it does over a VPN into their network).
    #[cfg(any(target_os = "macos", windows))]
    fn saved_section(&mut self, saved: &[(crate::viewer::saved::Computer, Option<std::net::IpAddr>)], cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = cx.theme().clone();
        let mut rows: Vec<AnyElement> = saved
            .iter()
            .map(|(c, at)| {
                let icon = match c.os.as_str() {
                    "macos" => I::Laptop,
                    "linux" => I::Server,
                    _ => I::Monitor,
                };
                let detail = match (at, c.addrs.first()) {
                    (Some(at), _) => format!("{at} · answers from here"),
                    (None, Some(last)) => format!("Not answering at {last}. On a VPN into its network it's one click; otherwise open it from your account or a link."),
                    (None, None) => "Not answering from here.".to_string(),
                };
                let id = c.id.clone();
                let forget = Button::new(SharedString::from(format!("forget-{}", c.id))).small().ghost().label("Forget").on_click(move |_, _, cx| {
                    crate::viewer::saved::forget(&id);
                    Store::refresh_saved(cx);
                });
                let control = h_flex()
                    .gap_2()
                    .when_some(*at, |d, at| {
                        let target = crate::viewer::Target { name: c.name.clone(), dns: c.url.clone(), is_self: false, token: None, link: None, remote: None, typed: None, at: Some(at) };
                        d.child(
                            Button::new(SharedString::from(format!("connect-saved-{}", c.id)))
                                .small()
                                .primary()
                                .label("Connect")
                                .icon(I::Monitor)
                                .on_click(cx.listener(move |this, _, window, cx| this.connect(target.clone(), window, cx))),
                        )
                    })
                    .child(forget);
                ui::icon_row(icon, if at.is_some() { theme.success } else { theme.muted_foreground }, c.name.clone(), detail, control, cx)
            })
            .collect();
        if rows.is_empty() {
            rows.push(ui::note(
                "Computers you open are remembered here, to open again from wherever this one is. Over a VPN into their network they can't show up by themselves, and here they're still one click.",
                cx,
            ));
        }
        Some(ui::section("Opened before", rows, cx).into_any_element())
    }

    #[cfg(not(any(target_os = "macos", windows)))]
    fn saved_section(&mut self, _: &[(crate::viewer::saved::Computer, Option<std::net::IpAddr>)], _: &mut Context<Self>) -> Option<AnyElement> {
        None
    }

    /// Somebody else's computer, from a link they sent, or any computer by its address.
    #[cfg(any(target_os = "macos", windows))]
    fn link_section(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        use gpui_kit::component::input::Input;
        let control = h_flex()
            .gap_2()
            .w(super::text_size::pt(420.))
            .child(div().flex_1().child(Input::new(&self.open_link).small()))
            .child(Button::new("open-link").small().primary().label("Connect").on_click(cx.listener(|this, _, window, cx| {
                let url = this.open_link.read(cx).value().to_string();
                let Some(target) = crate::viewer::Target::from_url(&url) else {
                    return super::toast(window, cx, Err(anyhow::anyhow!("That doesn't look like a Glass link or an address.")));
                };
                // An address is looked up first: which computer is there, and what it's
                // remembered by. (A moment, off the main thread; a link comes straight back.)
                cx.spawn_in(window, async move |this, cx| {
                    let settled = cx.background_executor().spawn(async move { target.settle() }).await;
                    let _ = this.update_in(cx, |this, window, cx| match settled {
                        Ok(target) => this.connect(target, window, cx),
                        Err(e) => super::toast(window, cx, Err(e)),
                    });
                })
                .detach();
            })));
        Some(
            ui::section(
                "Open a link or an address",
                vec![ui::row("A link someone sent you, or a computer's address", "An address (192.168.1.20) works on its network and over a VPN into it. It's on that computer's Home, under This network or VPN.", control, cx)],
                cx,
            )
            .into_any_element(),
        )
    }

    #[cfg(not(any(target_os = "macos", windows)))]
    fn link_section(&mut self, _: &mut Context<Self>) -> Option<AnyElement> {
        None
    }
}
