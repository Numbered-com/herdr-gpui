use gpui::{KeyDownEvent, Keystroke, Modifiers, ScrollDelta, ScrollWheelEvent, TouchPhase};
use herdr_client::protocol::{
    CellData, ClientKeyCode, ClientKeyKind, ClientMouseGeometry, ClientMouseKind,
    ClientMousePosition, ClientPaneInputEvent, ClientSurfaceSize, PaneSurfaceFrame, SurfaceRect,
};

pub const BACKGROUND: u32 = 0x101419;
pub const FOREGROUND: u32 = 0xd8dee9;
pub const FONT_SIZE: f32 = 14.;
pub const CELL_HEIGHT: f32 = 20.;

#[derive(Default)]
pub struct WheelAccumulator {
    target: Option<(String, bool)>,
    remainder: f32,
}

impl WheelAccumulator {
    pub fn lines(&mut self, id: &str, popup: bool, event: &ScrollWheelEvent) -> i16 {
        let target = (id.to_owned(), popup);
        if self.target.as_ref() != Some(&target) || matches!(event.touch_phase, TouchPhase::Started)
        {
            self.remainder = 0.;
            self.target = Some(target);
        }
        let delta = match event.delta {
            ScrollDelta::Pixels(delta) => delta.y.to_f64() as f32 / CELL_HEIGHT,
            ScrollDelta::Lines(delta) => delta.y,
        };
        if !delta.is_finite() {
            return 0;
        }
        if delta != 0. && delta.signum() != self.remainder.signum() {
            self.remainder = 0.;
        }
        // Bound each event's work; keep sub-cell trackpad motion, not an input backlog.
        let total = (self.remainder + delta).clamp(-128., 128.);
        let lines = total.trunc() as i16;
        self.remainder = total - f32::from(lines);
        lines
    }
}

pub struct WheelTarget {
    pub id: String,
    pub popup: bool,
    position: ClientMousePosition,
    geometry: Option<ClientMouseGeometry>,
}

impl WheelTarget {
    pub fn event(&self, lines: i16, modifiers: Modifiers) -> ClientPaneInputEvent {
        ClientPaneInputEvent::Mouse {
            kind: if lines > 0 {
                ClientMouseKind::ScrollUp
            } else {
                ClientMouseKind::ScrollDown
            },
            position: self.position,
            geometry: self.geometry,
            modifiers: u8::from(modifiers.shift)
                | (u8::from(modifiers.control) << 1)
                | (u8::from(modifiers.alt) << 2)
                | (u8::from(modifiers.platform) << 3),
            lines: lines.unsigned_abs(),
        }
    }
}

pub fn wheel_target(
    surface: &PaneSurfaceFrame,
    x: f32,
    y: f32,
    cell_width: f32,
) -> Option<WheelTarget> {
    if !x.is_finite() || !y.is_finite() || cell_width <= 0. || x < 0. || y < 0. {
        return None;
    }
    let (id, popup, rect, pixel_mouse, width_px, height_px, origin_x, origin_y) =
        if let Some(popup) = &surface.popup {
            let origin_x =
                (surface.frame.width.saturating_sub(popup.frame.width) as f32 * cell_width / 2.)
                    .floor();
            let origin_y =
                surface.frame.height.saturating_sub(popup.frame.height) as f32 * CELL_HEIGHT / 2.;
            (
                &popup.terminal_id,
                true,
                SurfaceRect {
                    x: 0,
                    y: 0,
                    width: popup.frame.width,
                    height: popup.frame.height,
                },
                popup.sgr_pixel_mouse,
                popup.pixel_width,
                popup.pixel_height,
                origin_x,
                origin_y,
            )
        } else {
            let pane = surface.panes.iter().find(|pane| {
                let r = pane.inner_rect;
                x >= r.x as f32 * cell_width
                    && x < (u32::from(r.x) + u32::from(r.width)) as f32 * cell_width
                    && y >= r.y as f32 * CELL_HEIGHT
                    && y < (u32::from(r.y) + u32::from(r.height)) as f32 * CELL_HEIGHT
            })?;
            (
                &pane.pane_id,
                false,
                pane.inner_rect,
                pane.sgr_pixel_mouse,
                pane.pixel_width,
                pane.pixel_height,
                pane.inner_rect.x as f32 * cell_width,
                pane.inner_rect.y as f32 * CELL_HEIGHT,
            )
        };
    let x = x - origin_x;
    let y = y - origin_y;
    if x < 0.
        || y < 0.
        || x >= rect.width as f32 * cell_width
        || y >= rect.height as f32 * CELL_HEIGHT
    {
        return None;
    }
    let column = (x / cell_width).floor() as u16;
    let row = (y / CELL_HEIGHT).floor() as u16;
    let geometry = (pixel_mouse && width_px > 0 && height_px > 0).then_some(ClientMouseGeometry {
        cols: rect.width,
        rows: rect.height,
        width_px,
        height_px,
    });
    let position = if geometry.is_some() {
        ClientMousePosition::Pixels {
            x: (x / (rect.width as f32 * cell_width) * width_px as f32).floor() as u32,
            y: (y / (rect.height as f32 * CELL_HEIGHT) * height_px as f32).floor() as u32,
            column,
            row,
        }
    } else {
        ClientMousePosition::Cell { column, row }
    };
    Some(WheelTarget {
        id: id.clone(),
        popup,
        position,
        geometry,
    })
}

const ANSI: [u32; 16] = [
    0x000000, 0x800000, 0x008000, 0x808000, 0x000080, 0x800080, 0x008080, 0xc0c0c0, 0x808080,
    0xff0000, 0x00ff00, 0xffff00, 0x0000ff, 0xff00ff, 0x00ffff, 0xffffff,
];

pub fn color(value: u32, default: u32) -> u32 {
    match value >> 24 {
        0 => match value & 255 {
            1..=16 => ANSI[((value & 255) - 1) as usize],
            _ => default,
        },
        1 => match value & 255 {
            n @ 0..=15 => ANSI[n as usize],
            n @ 16..=231 => {
                let n = n - 16;
                let level = |v| if v == 0 { 0 } else { 55 + v * 40 };
                (level(n / 36) << 16) | (level(n / 6 % 6) << 8) | level(n % 6)
            }
            n => (8 + (n - 232) * 10) * 0x010101,
        },
        2 => value & 0xffffff,
        _ => default,
    }
}

pub fn cell_colors(cell: &CellData) -> (u32, u32) {
    let mut fg = color(cell.fg, FOREGROUND);
    let mut bg = color(cell.bg, BACKGROUND);
    if cell.modifier & (1 << 6) != 0 {
        std::mem::swap(&mut fg, &mut bg);
    }
    if cell.modifier & (1 << 1) != 0 {
        fg = ((fg & 0xfefefe) >> 1) + ((bg & 0xfefefe) >> 1);
    }
    if cell.modifier & (1 << 7) != 0 {
        fg = bg;
    }
    (fg, bg)
}

pub fn viewport(width: f32, height: f32, cell_width: f32) -> ClientSurfaceSize {
    let cols = (width / cell_width.max(1.)).floor().clamp(1., 4096.) as u16;
    let rows = (height / CELL_HEIGHT).floor().clamp(1., 4096.) as u16;
    ClientSurfaceSize {
        cols,
        rows: rows.min((1_000_000 / u32::from(cols)) as u16),
    }
}

// Printable text belongs to EntityInputHandler, not key-down: this preserves
// keyboard layouts, dead keys and IME commits without double-sending characters.
pub fn key_input(event: &KeyDownEvent) -> Option<ClientPaneInputEvent> {
    key_code(&event.keystroke).map(|code| ClientPaneInputEvent::Key {
        code,
        modifiers: u8::from(event.keystroke.modifiers.shift)
            | (u8::from(event.keystroke.modifiers.control) << 1)
            | (u8::from(event.keystroke.modifiers.alt) << 2),
        kind: if event.is_held {
            ClientKeyKind::Repeat
        } else {
            ClientKeyKind::Press
        },
        repeat_count: 1,
        shifted_codepoint: None,
        generated_text: None,
        tracks_release: false,
        physical_key_id: None,
        windows_record: None,
    })
}

fn key_code(key: &Keystroke) -> Option<ClientKeyCode> {
    use ClientKeyCode::*;
    if key.modifiers.platform {
        return None;
    }
    Some(match key.key.as_str() {
        "enter" => Enter,
        "backspace" | "back" => Backspace,
        "escape" => Esc,
        "tab" if key.modifiers.shift => BackTab,
        "tab" => Tab,
        "up" => Up,
        "down" => Down,
        "left" => Left,
        "right" => Right,
        "home" => Home,
        "end" => End,
        "pageup" => PageUp,
        "pagedown" => PageDown,
        "delete" => Delete,
        "insert" => Insert,
        name if name.starts_with('f')
            && name[1..].parse::<u8>().is_ok_and(|n| (1..=24).contains(&n)) =>
        {
            F(name[1..].parse().ok()?)
        }
        "space" if key.modifiers.control => Char(' '),
        name if key.modifiers.control && name.chars().count() == 1 => Char(name.chars().next()?),
        _ => return None,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn wheel_preserves_fractions_and_resets_on_target_direction_or_gesture_change() {
        let mut wheel = WheelAccumulator::default();
        let mut event = ScrollWheelEvent {
            delta: ScrollDelta::Pixels(gpui::point(gpui::px(0.), gpui::px(12.))),
            touch_phase: TouchPhase::Moved,
            ..Default::default()
        };
        assert_eq!(wheel.lines("pane", false, &event), 0);
        assert_eq!(wheel.lines("pane", false, &event), 1);
        assert_eq!(wheel.lines("other", false, &event), 0);
        assert_eq!(wheel.lines("other", true, &event), 0);
        event.touch_phase = TouchPhase::Started;
        assert_eq!(wheel.lines("other", true, &event), 0);
        event.touch_phase = TouchPhase::Moved;
        event.delta = ScrollDelta::Lines(gpui::point(0., -1.));
        assert_eq!(wheel.lines("other", true, &event), -1);
        event.delta = ScrollDelta::Lines(gpui::point(0., 1e9));
        assert_eq!(wheel.lines("other", true, &event), 128);
        event.delta = ScrollDelta::Lines(gpui::point(10., 0.));
        assert_eq!(wheel.lines("other", true, &event), 0);
    }

    #[test]
    fn nonfinite_wheel_deltas_do_not_poison_fractional_motion() {
        for invalid in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut wheel = WheelAccumulator::default();
            let mut event = ScrollWheelEvent {
                delta: ScrollDelta::Lines(gpui::point(0., 0.75)),
                touch_phase: TouchPhase::Moved,
                ..Default::default()
            };
            assert_eq!(wheel.lines("pane", false, &event), 0);
            event.delta = ScrollDelta::Lines(gpui::point(0., invalid));
            assert_eq!(wheel.lines("pane", false, &event), 0);
            event.delta = ScrollDelta::Lines(gpui::point(0., 0.25));
            assert_eq!(wheel.lines("pane", false, &event), 1);
            event.delta = ScrollDelta::Lines(gpui::point(0., -1e9));
            assert_eq!(wheel.lines("pane", false, &event), -128);
            event.delta = ScrollDelta::Lines(gpui::point(0., 0.));
            assert_eq!(wheel.lines("pane", false, &event), 0);
        }
    }

    #[test]
    fn wheel_hits_inner_pane_and_uses_relative_coordinates_and_semantic_modes() {
        use herdr_client::protocol::*;
        let frame = FrameData {
            cells: vec![],
            width: 80,
            height: 24,
            cursor: None,
            hyperlinks: vec![],
            graphics: vec![],
        };
        let mut surface = PaneSurfaceFrame {
            boot_id: "boot".into(),
            projection_revision: 1,
            surface_revision: 1,
            frame: frame.clone(),
            splits: vec![],
            popup: None,
            graphics: Default::default(),
            panes: vec![PaneSurfacePane {
                pane_id: "pane".into(),
                content_revision: 1,
                rect: SurfaceRect {
                    x: 0,
                    y: 0,
                    width: 40,
                    height: 24,
                },
                inner_rect: SurfaceRect {
                    x: 1,
                    y: 1,
                    width: 38,
                    height: 22,
                },
                scrollbar_rect: None,
                scroll: None,
                focused: false,
                mouse_reporting: false,
                sgr_pixel_mouse: false,
                alternate_screen_active: false,
                pixel_width: 380,
                pixel_height: 440,
            }],
        };
        assert!(wheel_target(&surface, -1., 25., 10.).is_none());
        assert!(wheel_target(&surface, 5., 25., 10.).is_none());
        assert!(wheel_target(&surface, 400., 25., 10.).is_none());
        for alternate in [false, true] {
            surface.panes[0].alternate_screen_active = alternate;
            let target = wheel_target(&surface, 35., 65., 10.).unwrap();
            assert_eq!(target.id, "pane");
            assert_eq!(
                target.position,
                ClientMousePosition::Cell { column: 2, row: 2 }
            );
            assert!(matches!(
                target.event(-3, Modifiers::default()),
                ClientPaneInputEvent::Mouse {
                    kind: ClientMouseKind::ScrollDown,
                    lines: 3,
                    ..
                }
            ));
        }
        surface.panes[0].sgr_pixel_mouse = true;
        let target = wheel_target(&surface, 35., 65., 10.).unwrap();
        assert_eq!(
            target.position,
            ClientMousePosition::Pixels {
                x: 25,
                y: 45,
                column: 2,
                row: 2
            }
        );
        assert_eq!(target.geometry.unwrap().cols, 38);
        surface.popup = Some(Box::new(ClientShellPopupSurface {
            terminal_id: "popup".into(),
            title: String::new(),
            width: None,
            height: None,
            frame: FrameData {
                width: 20,
                height: 10,
                ..frame
            },
            mouse_reporting: true,
            sgr_pixel_mouse: false,
            pixel_width: 200,
            pixel_height: 200,
        }));
        assert!(wheel_target(&surface, 35., 65., 10.).is_none());
        let target = wheel_target(&surface, 315., 165., 10.).unwrap();
        assert!(target.popup);
        assert_eq!(target.id, "popup");
        assert_eq!(
            target.position,
            ClientMousePosition::Cell { column: 1, row: 1 }
        );
    }

    #[test]
    fn wire_colors_are_not_argb() {
        assert_eq!(color(0, FOREGROUND), FOREGROUND);
        assert_eq!(color(0, BACKGROUND), BACKGROUND);
        for (i, expected) in ANSI.iter().enumerate() {
            assert_eq!(color(i as u32 + 1, 0), *expected);
            assert_eq!(color(0x01000000 | i as u32, 0), *expected);
        }
        assert_eq!(color(0x02123456, 0), 0x123456);
        assert_eq!(color(0x01000010, 1), 0);
        assert_eq!(color(0x01000015, 0), 0x0000ff);
        assert_eq!(color(0x010000e7, 0), 0xffffff);
        assert_eq!(color(0x010000e8, 0), 0x080808);
        assert_eq!(color(0x010000ff, 0), 0xeeeeee);
        assert_eq!(color(0xff000000, 42), 42);
    }

    #[test]
    fn reverse_and_hidden_colors() {
        let mut cell = CellData {
            symbol: "x".into(),
            fg: 0x02ff0000,
            bg: 0x020000ff,
            modifier: 1 << 6,
            skip: false,
            hyperlink: None,
        };
        assert_eq!(cell_colors(&cell), (0x0000ff, 0xff0000));
        cell.modifier |= 1 << 7;
        assert_eq!(cell_colors(&cell), (0xff0000, 0xff0000));
    }

    #[test]
    fn geometry_is_bounded_and_uses_terminal_viewport() {
        assert_eq!(
            viewport(800., 480., 10.),
            ClientSurfaceSize { cols: 80, rows: 24 }
        );
        assert_eq!(
            viewport(0., 0., 10.),
            ClientSurfaceSize { cols: 1, rows: 1 }
        );
        let huge = viewport(1e9, 1e9, 10.);
        assert!(u32::from(huge.cols) * u32::from(huge.rows) <= 1_000_000);
    }

    #[test]
    fn special_keys_and_text_are_separate() {
        let key = |s| Keystroke::parse(s).unwrap();
        assert_eq!(key_code(&key("ctrl-c")), Some(ClientKeyCode::Char('c')));
        assert_eq!(key_code(&key("shift-tab")), Some(ClientKeyCode::BackTab));
        assert_eq!(key_code(&key("alt-left")), Some(ClientKeyCode::Left));
        assert_eq!(key_code(&key("f12")), Some(ClientKeyCode::F(12)));
        assert_eq!(key_code(&key("a")), None);
        assert_eq!(key_code(&key("alt-e")), None);
        assert_eq!(key_code(&key("cmd-q")), None);
    }
}
