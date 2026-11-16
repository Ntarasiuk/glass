//! A shell on the other computer (owner only), drawn natively: the host's pty output goes
//! through a VT100 parser, and its screen is drawn as monospace text.

use base64::Engine;
use gpui_kit::component::ActiveTheme;
use gpui_kit::prelude::*;
use gpui_kit::*;
use serde_json::{Value, json};

const CELL_W: f32 = 7.83;
const CELL_H: f32 = 17.0;
const FONT_SIZE: f32 = 13.0;

pub struct TermView {
    parser: vt100::Parser,
    send: Box<dyn Fn(Value)>,
    pub(super) focus: FocusHandle,
    size: (u16, u16),
    open: bool,
    exited: Option<Option<i64>>,
}

/// xterm's 16 colours, then the 6×6×6 cube and the greys.
fn indexed(i: u8) -> Hsla {
    const BASE: [u32; 16] = [
        0x1e1e1e, 0xcd3131, 0x0dbc79, 0xe5e510, 0x2472c8, 0xbc3fbc, 0x11a8cd, 0xe5e5e5, 0x666666, 0xf14c4c, 0x23d18b, 0xf5f543, 0x3b8eea, 0xd670d6, 0x29b8db, 0xffffff,
    ];
    let rgb = match i {
        0..=15 => BASE[i as usize],
        16..=231 => {
            let i = i - 16;
            let c = |v: u8| if v == 0 { 0 } else { 55 + v as u32 * 40 };
            (c(i / 36) << 16) | (c((i / 6) % 6) << 8) | c(i % 6)
        }
        _ => {
            let g = 8 + (i as u32 - 232) * 10;
            (g << 16) | (g << 8) | g
        }
    };
    gpui_kit::rgb(rgb).into()
}

impl TermView {
    pub fn new(send: impl Fn(Value) + 'static, cx: &mut Context<Self>) -> Self {
        Self { parser: vt100::Parser::new(24, 80, 2000), send: Box::new(send), focus: cx.focus_handle(), size: (24, 80), open: false, exited: None }
    }

    /// A `term` message from the host.
    pub fn on_message(&mut self, msg: &Value, cx: &mut Context<Self>) {
        match msg["op"].as_str() {
            Some("output") => {
                if let Some(bytes) = msg["data"].as_str().and_then(|d| base64::engine::general_purpose::STANDARD.decode(d).ok()) {
                    self.parser.process(&bytes);
                }
            }
            Some("exit") => {
                self.exited = Some(msg["code"].as_i64());
                self.open = false;
            }
            _ => {}
        }
        cx.notify();
    }

    fn write(&self, bytes: &[u8]) {
        (self.send)(json!({ "t": "term", "op": "input", "data": base64::engine::general_purpose::STANDARD.encode(bytes) }));
    }

    pub fn close(&mut self) {
        if self.open {
            (self.send)(json!({ "t": "term", "op": "close" }));
            self.open = false;
        }
    }

    fn resized(&mut self, bounds: Bounds<Pixels>) {
        let cols = ((f32::from(bounds.size.width) - 16.) / CELL_W).floor().max(10.) as u16;
        let rows = ((f32::from(bounds.size.height) - 12.) / CELL_H).floor().max(3.) as u16;
        if (rows, cols) == self.size && self.open {
            return;
        }
        self.size = (rows, cols);
        self.parser.screen_mut().set_size(rows, cols);
        if self.open {
            (self.send)(json!({ "t": "term", "op": "resize", "cols": cols, "rows": rows }));
        } else if self.exited.is_none() {
            (self.send)(json!({ "t": "term", "op": "open", "cols": cols, "rows": rows }));
            self.open = true;
        }
    }

    fn key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) {
        let k = &ev.keystroke;
        let m = &k.modifiers;
        if m.platform {
            if k.key == "v" {
                let (text, _, _) = crate::viewer::sys::clipboard();
                let text = text.replace("\r\n", "\r").replace('\n', "\r");
                if self.parser.screen().bracketed_paste() {
                    self.write(format!("\x1b[200~{text}\x1b[201~").as_bytes());
                } else {
                    self.write(text.as_bytes());
                }
                cx.stop_propagation();
            }
            return;
        }
        let app = self.parser.screen().application_cursor();
        let arrow = |c: char| if app { format!("\x1bO{c}") } else { format!("\x1b[{c}") };
        let seq: Option<String> = match k.key.as_str() {
            "enter" => Some("\r".into()),
            "backspace" => Some("\x7f".into()),
            "tab" => Some(if m.shift { "\x1b[Z".into() } else { "\t".into() }),
            "escape" => Some("\x1b".into()),
            "up" => Some(arrow('A')),
            "down" => Some(arrow('B')),
            "right" => Some(arrow('C')),
            "left" => Some(arrow('D')),
            "home" => Some("\x1b[H".into()),
            "end" => Some("\x1b[F".into()),
            "delete" => Some("\x1b[3~".into()),
            "pageup" => Some("\x1b[5~".into()),
            "pagedown" => Some("\x1b[6~".into()),
            key if m.control && key.len() == 1 => {
                let c = key.as_bytes()[0].to_ascii_lowercase();
                matches!(c, b'a'..=b'z' | b'[' | b'\\' | b']').then(|| ((c & 0x1f) as char).to_string())
            }
            _ => k.key_char.clone().map(|t| if m.alt { format!("\x1b{t}") } else { t }),
        };
        if let Some(s) = seq {
            self.write(s.as_bytes());
        }
        cx.stop_propagation();
    }

    fn row(&self, r: u16, theme: &gpui_kit::component::Theme) -> StyledText {
        let screen = self.parser.screen();
        let (cursor_r, cursor_c) = screen.cursor_position();
        let show_cursor = !screen.hide_cursor() && cursor_r == r && self.open;
        let mut text = String::new();
        let mut runs: Vec<(std::ops::Range<usize>, HighlightStyle)> = Vec::new();
        for c in 0..self.size.1 {
            let Some(cell) = screen.cell(r, c) else { break };
            if cell.is_wide_continuation() {
                continue;
            }
            let start = text.len();
            let s = cell.contents();
            text.push_str(if s.is_empty() { " " } else { s });
            let color = |c: vt100::Color, default: Hsla| match c {
                vt100::Color::Default => default,
                vt100::Color::Idx(i) => indexed(i),
                vt100::Color::Rgb(r, g, b) => gpui_kit::rgb(((r as u32) << 16) | ((g as u32) << 8) | b as u32).into(),
            };
            let (mut fg, mut bg) = (color(cell.fgcolor(), theme.foreground), color(cell.bgcolor(), gpui_kit::transparent_black()));
            if cell.inverse() ^ (show_cursor && c == cursor_c) {
                (fg, bg) = (if bg.a == 0. { theme.background } else { bg }, fg);
            }
            let style = HighlightStyle {
                color: Some(fg),
                background_color: (bg.a > 0.).then_some(bg),
                font_weight: cell.bold().then_some(FontWeight::BOLD),
                ..Default::default()
            };
            match runs.last_mut() {
                Some((range, last)) if *last == style => range.end = text.len(),
                _ => runs.push((start..text.len(), style)),
            }
        }
        StyledText::new(text).with_highlights(runs)
    }
}

impl Render for TermView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let this = cx.entity().downgrade();
        div()
            .id("remote-term")
            .relative()
            .size_full()
            .px_2()
            .py_1p5()
            .bg(theme.background)
            .font_family("Menlo")
            .text_size(px(FONT_SIZE))
            .line_height(px(CELL_H))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| this.key(ev, cx)))
            .on_click(cx.listener(|this, _, window, cx| window.focus(&this.focus, cx)))
            .children((0..self.size.0).map(|r| div().h(px(CELL_H)).whitespace_nowrap().overflow_hidden().child(self.row(r, &theme))))
            .when_some(self.exited, |d, code| {
                d.child(div().text_color(theme.muted_foreground).child(format!("The shell ended{}.", code.map_or(String::new(), |c| format!(" ({c})")))))
            })
            .child(
                canvas(
                    move |bounds, _, cx| {
                        let _ = this.update(cx, |v, _| v.resized(bounds));
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            )
    }
}
