//! Semantic input on its way to a pane or popup: keystrokes, paste, and wheel
//! deltas. Nothing here synthesizes terminal keys for an operation that has an
//! endpoint API method, and nothing is sent while a menu page holds input.

use super::HerdrWindow;
use crate::{
    connection::ConnectionBridge,
    terminal::{InputTarget, WheelAccumulator, key_input, wheel_target},
};
use gpui::{Context, KeyDownEvent, ScrollWheelEvent, Window};
use herdr_client::protocol::ClientPaneInputEvent;

impl HerdrWindow {
    pub(crate) fn send(&mut self, event: ClientPaneInputEvent, cx: &mut Context<Self>) {
        if self.menu.page.is_some() || !self.input_ready() {
            return;
        }
        if let (Some(handle), Some(snapshot), Some(surface)) = (
            &self.endpoints[self.selected_endpoint].connection.handle,
            &self.live.snapshot,
            &self.live.surface,
        ) {
            let target = if let Some(popup) = &surface.popup {
                InputTarget::Popup(popup.terminal_id.clone())
            } else if let Some(pane) = &snapshot.focused_pane_id {
                InputTarget::Pane(pane.clone())
            } else {
                return;
            };
            if let Err(error) =
                ConnectionBridge::send_input(handle, &snapshot.boot_id, &target, event)
            {
                self.local_error = Some(format!("Input not sent: {error}"));
                cx.notify();
            }
        }
    }

    pub(crate) fn scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.menu.page.is_some() || !self.input_ready() {
            return;
        }
        let (Some(handle), Some(snapshot), Some(surface)) = (
            &self.endpoints[self.selected_endpoint].connection.handle,
            &self.live.snapshot,
            &self.live.surface,
        ) else {
            return;
        };
        let x = (event.position.x - self.bounds.origin.x).to_f64() as f32;
        let y = (event.position.y - self.bounds.origin.y).to_f64() as f32;
        let cell_height = self.config.terminal.line_height();
        let Some(target) = wheel_target(surface, x, y, self.cell_width, cell_height) else {
            self.wheel = WheelAccumulator::default();
            return;
        };
        let lines = self.wheel.lines(&target.target, event, cell_height);
        cx.stop_propagation();
        if lines == 0 {
            return;
        }
        let input = target.event(lines, event.modifiers);
        let result = ConnectionBridge::send_input(handle, &snapshot.boot_id, &target.target, input);
        if let Err(error) = result {
            self.local_error = Some(format!("Wheel input not sent: {error}"));
            cx.notify();
        }
    }

    pub(crate) fn key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        #[cfg(feature = "integration-test")]
        {
            self.input_probe.keys += 1;
        }
        if event.keystroke.modifiers.platform && event.keystroke.key == "v" {
            if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                self.send(ClientPaneInputEvent::Paste(text), cx);
            }
            cx.stop_propagation();
            window.prevent_default();
        } else if self.marked.is_empty()
            && let Some(input) = key_input(event)
        {
            self.send(input, cx);
            cx.stop_propagation();
            window.prevent_default();
        }
    }
}
