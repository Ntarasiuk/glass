//! AI agents: add Glass to Claude Code, Claude Desktop or Cursor in one click, steer an
//! agent at work (pause, stop), and answer its questions.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{ActiveTheme, Icon, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::*;

use super::actions::{self, Client};
use super::ui::{self, I};
use super::{GlassApp, act};

impl GlassApp {
    /// Which MCP clients are here and already have Glass (reads their configs, off the
    /// main thread).
    pub(super) fn check_clients(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let clients = cx
                .background_executor()
                .spawn(async { Client::ALL.into_iter().map(|c| (c, c.present(), c.present() && c.added())).collect::<Vec<_>>() })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.clients = clients;
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn agents(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let snap = self.store.read(cx).snap.clone();
        let theme = cx.theme().clone();
        let agent = &snap.agent;

        let (state, tint) = if agent.stopped {
            ("Stopped: agents can't act until you allow them again.", theme.danger)
        } else if agent.paused {
            ("Paused: an agent's next step waits for you.", theme.warning)
        } else {
            ("Allowed: agents you've added can use this computer while you watch.", theme.success)
        };
        let steer = ui::icon_row(
            I::Bot,
            tint,
            "Agents on this computer",
            state,
            h_flex()
                .gap_2()
                .when(!agent.stopped, |d| {
                    let op = if agent.paused { "resume" } else { "pause" };
                    d.child(
                        Button::new("pause")
                            .small()
                            .icon(if agent.paused { I::Play } else { I::Pause })
                            .label(if agent.paused { "Resume" } else { "Pause" })
                            .on_click(move |_, window, cx| act(window, cx, move || actions::agent(op))),
                    )
                })
                .child({
                    let op = if agent.stopped { "allow" } else { "stop" };
                    let b = Button::new("stop").small().label(if agent.stopped { "Allow again" } else { "Stop" });
                    let b = if agent.stopped { b.primary() } else { b.danger() };
                    b.on_click(move |_, window, cx| act(window, cx, move || actions::agent(op)))
                }),
            cx,
        );

        let asks: Vec<AnyElement> = agent
            .asks
            .iter()
            .map(|ask| {
                let id = ask.id;
                let help = ask.help;
                let note = self
                    .notes
                    .entry(id)
                    .or_insert_with(|| cx.new(|cx| InputState::new(window, cx).placeholder("A note for the agent (optional)")))
                    .clone();
                let n1 = note.clone();
                let n2 = note.clone();
                v_flex()
                    .p_4()
                    .gap_3()
                    .child(
                        h_flex()
                            .gap_3()
                            .child(ui::tile(if help { I::Hand } else { I::ShieldCheck }, theme.primary))
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(if help {
                                        "The agent needs your help"
                                    } else {
                                        "The agent asks before it acts"
                                    }))
                                    .child(div().text_sm().text_color(theme.muted_foreground).child(ask.text.clone())),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(div().flex_1().child(Input::new(&note).small()))
                            .child(Button::new(("no", id)).small().ghost().label(if help { "Can't" } else { "Don't" }).on_click(
                                move |_, window, cx| {
                                    let text = n1.read(cx).value().to_string();
                                    act(window, cx, move || actions::answer(id, false, &text))
                                },
                            ))
                            .child(Button::new(("yes", id)).small().primary().label(if help { "Done, hand back" } else { "Allow" }).on_click(
                                move |_, window, cx| {
                                    let text = n2.read(cx).value().to_string();
                                    act(window, cx, move || actions::answer(id, true, &text))
                                },
                            )),
                    )
                    .into_any_element()
            })
            .collect();

        let mut clients: Vec<AnyElement> = self
            .clients
            .iter()
            .map(|&(client, present, added)| {
                let control = if added {
                    ui::status("Added", theme.success).into_any_element()
                } else if !present {
                    div().text_xs().text_color(theme.muted_foreground).child("Not installed").into_any_element()
                } else {
                    Button::new(SharedString::from(format!("add-{}", client.name())))
                        .small()
                        .primary()
                        .icon(I::Plus)
                        .label("Add Glass")
                        .on_click(cx.listener(move |_, _, window, cx| {
                            cx.spawn_in(window, async move |this, cx| {
                                let r = cx.background_executor().spawn(async move { client.add() }).await;
                                let _ = this.update_in(cx, |this, window, cx| {
                                    super::toast(window, cx, r);
                                    this.check_clients(cx);
                                });
                            })
                            .detach();
                        }))
                        .into_any_element()
                };
                ui::icon_row(
                    match client {
                        Client::ClaudeCode => I::SquareTerminal,
                        _ => I::Sparkles,
                    },
                    theme.primary,
                    client.name(),
                    match client {
                        Client::ClaudeCode => "claude mcp add glass -- glass mcp",
                        Client::ClaudeDesktop => "Adds Glass to its MCP servers",
                        Client::Cursor => "Adds Glass to ~/.cursor/mcp.json",
                    },
                    control,
                    cx,
                )
            })
            .collect();
        if clients.is_empty() {
            clients.push(ui::note("Looking for agents on this computer…", cx));
        }
        clients.push(ui::icon_row(
            I::Clipboard,
            theme.muted_foreground,
            "Any other MCP client",
            "Copy the server config and paste it into the client's MCP settings.",
            Button::new("copy-mcp").small().icon(I::Copy).label("Copy config").on_click(|_, window, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(actions::mcp_json()));
                super::toast(window, cx, Ok("MCP config copied.".into()));
            }),
            cx,
        ));

        let how = v_flex()
            .p_5()
            .gap_3()
            .rounded(px(16.))
            .border_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .children(
                [
                    (I::Eye, "You see everything it does", "A banner in the viewer shows each step, with a marker where it clicks."),
                    (I::ShieldCheck, "It asks first", "Before sending, buying or deleting, it asks you to approve, here or in the viewer."),
                    (I::Network, "Any of your computers", "It can work on any machine on your tailnet that runs Glass."),
                ]
                .map(|(icon, title, line)| {
                    h_flex()
                        .gap_3()
                        .child(Icon::new(icon).small().text_color(theme.primary))
                        .child(
                            v_flex()
                                .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(title))
                                .child(div().text_xs().text_color(theme.muted_foreground).child(line)),
                        )
                }),
            );

        ui::page("AI agents", "Let Claude or another agent use this computer while you watch, pause or stop it.", cx)
            .when(!asks.is_empty(), |d| d.child(ui::section("Questions for you", asks, cx)))
            .child(ui::section("Connect an agent", clients, cx))
            .child(ui::section("Control", vec![steer], cx))
            .child(how)
    }
}
