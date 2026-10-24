//! Teams & SSO: sign-in with an identity provider or a directory, access rules for who
//! gets in and what they may do, provisioning, and device posture.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::Input;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::*;

use super::ui::{self, I};
use super::{GlassApp, act, actions};
use crate::access::Scope;

#[derive(Default)]
pub struct RuleForm {
    view_only: bool,
    ask: bool,
}

impl GlassApp {
    pub(super) fn teams(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let snap = self.store.read(cx).snap.clone();
        let theme = cx.theme().clone();
        let s = &snap.settings;
        let [oidc_secret, ldap_password, scim] = snap.secrets;

        let set = |on: bool| if on { "Set · type to replace" } else { "" };
        let sso = vec![
            self.field("oidc-issuer", "Provider", "Google, Okta, Microsoft Entra…", cx),
            self.field("oidc-client-id", "Client ID", "From the app you registered with the provider.", cx),
            self.field("oidc-client-secret", "Client secret", set(oidc_secret), cx),
            self.field("oidc-allow", "Who may sign in", "Emails, or @domain for everyone there.", cx),
            ui::row(
                "Signed-in people may",
                "Unless an access rule below says otherwise.",
                segmented(
                    "oidc-scope",
                    &["Use it", "Only watch"],
                    usize::from(s.oidc_view_only),
                    |i, window, cx| act(window, cx, move || actions::set_setting("oidc-scope", if i == 1 { "view-only" } else { "full" })),
                    cx,
                ),
                cx,
            ),
        ];

        let ldap = vec![
            self.field("ldap-url", "Server", "ldaps://, or ldap:// with StartTLS.", cx),
            self.field("ldap-user-dn", "User DN", "With {user} where the name goes.", cx),
            self.field("ldap-base-dn", "Or search under", "Where to look for the user.", cx),
            self.field("ldap-user-filter", "Search filter", "With {user}.", cx),
            self.field("ldap-bind-dn", "Search account", "Default: anonymous.", cx),
            self.field("ldap-bind-password", "Its password", set(ldap_password), cx),
            self.field("ldap-group-attr", "Groups attribute", "Default memberOf.", cx),
        ];

        let mut rules: Vec<AnyElement> = snap
            .rules
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let grant = r.duration_secs.map(|d| crate::access::describe(std::time::Duration::from_secs(d))).unwrap_or_else(|| "1 day".into());
                ui::icon_row(
                    if r.matcher.starts_with("group:") { I::Users } else { I::Mail },
                    theme.primary,
                    r.matcher.clone(),
                    format!("{} · signed in for {grant}{}", r.scope.describe(), if snap.rules_implicit { " · from Who may sign in" } else { "" }),
                    Button::new(("rm-rule", i)).small().ghost().icon(I::Trash).on_click(move |_, window, cx| act(window, cx, move || actions::remove_rule(i))),
                    cx,
                )
            })
            .collect();
        if rules.is_empty() {
            rules.push(ui::note("No rules: nobody can sign in with a directory account yet.", cx));
        }
        rules.push(
            v_flex()
                .px_4()
                .py_3()
                .gap_2()
                .child(
                    h_flex()
                        .gap_2()
                        .child(div().flex_1().child(Input::new(&self.rule_input).small()))
                        .child(Button::new("add-rule").small().primary().icon(I::Plus).label("Add rule").on_click(cx.listener(|this, _, window, cx| {
                            let m = this.rule_input.read(cx).value().to_string();
                            let scope = Scope { view_only: this.rule.view_only, ask: this.rule.ask, ..Scope::default() };
                            this.rule_input.update(cx, |s, cx| s.set_value("", window, cx));
                            act(window, cx, move || actions::add_rule(&m, scope, None));
                        }))),
                )
                .child(
                    h_flex()
                        .gap_5()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(Switch::new("rule-view").small().label("Only watch").checked(self.rule.view_only).on_click(cx.listener(|this, on: &bool, _, cx| {
                            this.rule.view_only = *on;
                            cx.notify();
                        })))
                        .child(Switch::new("rule-ask").small().label("Ask me each time").checked(self.rule.ask).on_click(cx.listener(|this, on: &bool, _, cx| {
                            this.rule.ask = *on;
                            cx.notify();
                        }))),
                )
                .into_any_element(),
        );

        let new_scim = self.new_scim.clone();
        let provisioning = vec![ui::row(
            "SCIM token",
            if scim { "Set. Your identity provider uses it at <this computer's address>/scim/v2." } else { "Let your identity provider add and remove people." },
            h_flex()
                .gap_2()
                .when(scim, |d| {
                    d.child(Button::new("scim-off").small().ghost().label("Remove").on_click(|_, window, cx| {
                        act(window, cx, || actions::set_setting("scim-token", ""))
                    }))
                })
                .child(Button::new("scim-new").small().label(if scim { "Replace…" } else { "Make a token" }).on_click(cx.listener(|_, _, window, cx| {
                    cx.spawn_in(window, async move |this, cx| {
                        let r = cx
                            .background_executor()
                            .spawn(async { actions::set_setting("scim-token", "new").map(|_| crate::settings::scim_token().unwrap_or_default()) })
                            .await;
                        let _ = this.update_in(cx, |this, window, cx| match r {
                            Ok(t) => {
                                this.new_scim = Some(t);
                                super::Store::refresh(cx);
                                cx.notify();
                            }
                            Err(e) => super::toast(window, cx, Err(e)),
                        });
                    })
                    .detach();
                }))),
            cx,
        )]
        .into_iter()
        .chain(new_scim.map(|t| {
            let copy = t.clone();
            h_flex()
                .px_4()
                .py_3()
                .gap_2()
                .bg(theme.primary.opacity(0.06))
                .child(div().flex_1().text_xs().text_color(theme.muted_foreground).child("Shown once. Paste it into your identity provider now."))
                .child(ui::mono(t).px_2().py_1().rounded(px(6.)).bg(theme.background))
                .child(Button::new("copy-scim").small().icon(I::Copy).on_click(cx.listener(move |this, _, window, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                    this.new_scim = None;
                    super::toast(window, cx, Ok("Token copied.".into()));
                })))
                .into_any_element()
        }))
        .collect();

        let posture = vec![
            self.field("posture-tailnet-tags", "Tailnet tags", "Other people's devices must carry one of these.", cx),
            self.field("posture-os", "Systems allowed", "macos, ios, windows, linux, android, chromeos.", cx),
            self.field("posture-min-browser", "Oldest browser", "Major version; 0 for any.", cx),
            self.toggle(
                "posture-require-passkey-uv",
                "Passkeys need Face ID or a PIN",
                "Not just a tap on a security key.",
                s.posture_passkey_uv,
                cx,
            ),
        ];

        ui::page("Teams & SSO", "Let colleagues sign in with your company's accounts, with rules for what they may do.", cx)
            .child(ui::section("Single sign-on", sso, cx))
            .child(ui::section("Access rules", rules, cx))
            .child(ui::section("Directory (LDAP)", ldap, cx))
            .child(ui::section("Provisioning", provisioning, cx))
            .child(ui::section("Device posture", posture, cx))
    }
}

/// A row of mutually exclusive choices.
pub fn segmented(
    id: &'static str,
    labels: &[&'static str],
    selected: usize,
    on: impl Fn(usize, &mut Window, &mut App) + 'static,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    let on = std::rc::Rc::new(on);
    h_flex().gap_1().p_0p5().rounded(px(9.)).bg(theme.secondary_hover).children(labels.iter().enumerate().map(|(i, label)| {
        let active = i == selected;
        let on = on.clone();
        div()
            .id((id, i))
            .px_3()
            .py_1()
            .rounded(px(7.))
            .text_xs()
            .font_weight(FontWeight::MEDIUM)
            .cursor_pointer()
            .when(active, |d| d.bg(theme.background).shadow_sm())
            .when(!active, |d| d.text_color(theme.muted_foreground))
            .child(*label)
            .on_click(move |_, window, cx| {
                if !active {
                    on(i, window, cx)
                }
            })
    }))
}
