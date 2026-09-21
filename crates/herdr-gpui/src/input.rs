use super::{HerdrWindow, terminal::input_cursor_bounds};
use gpui::*;
use herdr_client::protocol::ClientPaneInputEvent;
use std::ops::Range;

impl EntityInputHandler for HerdrWindow {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        if self.menu.page.is_some() {
            let input = self.menu.input.as_ref()?;
            let range = input.range_from_utf16(range);
            *adjusted = Some(input.to_utf16(range.clone()));
            return Some(input.text[range].to_owned());
        }
        let text: Vec<u16> = self.marked.encode_utf16().collect();
        let range = range.start.min(text.len())..range.end.min(text.len());
        *adjusted = Some(range.clone());
        Some(String::from_utf16_lossy(&text[range]))
    }
    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        if self.menu.page.is_some() {
            let input = self.menu.input.as_ref()?;
            return Some(UTF16Selection {
                range: input.to_utf16(input.selection.clone()),
                reversed: input.reversed,
            });
        }
        let end = self.marked.encode_utf16().count();
        Some(UTF16Selection {
            range: end..end,
            reversed: false,
        })
    }
    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        if self.menu.page.is_some() {
            let input = self.menu.input.as_ref()?;
            return input.marked.clone().map(|range| input.to_utf16(range));
        }
        (!self.marked.is_empty()).then(|| 0..self.marked.encode_utf16().count())
    }
    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(input) = self.menu.input.as_mut() {
            input.marked = None;
        }
        self.marked.clear();
        cx.notify();
    }
    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.menu.page.is_some() {
            if let Some(input) = self.menu.input.as_mut() {
                input.replace(range, text, false, None);
                cx.notify();
            }
            return;
        }
        #[cfg(feature = "integration-test")]
        {
            self.input_probe.text += 1;
        }
        self.marked.clear();
        if !text.is_empty() {
            self.send(ClientPaneInputEvent::TextCommit(text.into()), cx);
        }
        cx.notify();
    }
    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.menu.page.is_some() {
            if let Some(input) = self.menu.input.as_mut() {
                input.replace(range, text, true, selected);
                cx.notify();
            }
            return;
        }
        if !self.input_ready() {
            return;
        }
        self.marked = text.into();
        cx.notify();
    }
    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        if self.menu.page.is_some() {
            return self.menu.input.as_ref()?.range_bounds(range);
        }
        Some(input_cursor_bounds(
            self.live.surface.as_deref(),
            self.bounds.origin,
            self.cell_width,
            self.config.terminal.line_height(),
        ))
    }
    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        if self.menu.page.is_some() {
            let input = self.menu.input.as_ref()?;
            let index = input.index_at(point)?;
            return Some(input.to_utf16(index..index).start);
        }
        None
    }
}
