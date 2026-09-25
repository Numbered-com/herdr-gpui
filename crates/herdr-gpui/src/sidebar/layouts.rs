//! The row layouts the config can pick. Each one is a unit struct, so adding
//! a layout is a new file and a `RowStyle` variant.

mod herdr;
mod orca;
mod superset;

pub(super) use {herdr::Herdr, orca::Orca, superset::Superset};

use super::{cell::Fold, label_text};
use crate::config::Theme;
use gpui::{prelude::*, *};

impl Fold {
    /// The fold chevron, sized by the caller. Clicking it never selects the
    /// row it sits on.
    pub(super) fn element(self, theme: &Theme) -> Stateful<Div> {
        let Self {
            id,
            index,
            collapsed,
            toggle,
        } = self;
        let (muted, foreground) = (theme.muted, theme.foreground);
        div()
            .id(id)
            .debug_selector(move || format!("collapse-{index}"))
            .flex_none()
            .text_color(rgb(muted))
            .hover(move |style| style.text_color(rgb(foreground)))
            .cursor_pointer()
            .child(label_text(if collapsed { "\u{25b8}" } else { "\u{25be}" }))
            .on_click(move |event, window, cx| {
                cx.stop_propagation();
                toggle(event, window, cx);
            })
    }
}

/// `color` at `alpha` out of 255, for washes laid over the sidebar surface.
fn wash(color: u32, alpha: u8) -> Rgba {
    rgba((color << 8) | u32::from(alpha))
}
