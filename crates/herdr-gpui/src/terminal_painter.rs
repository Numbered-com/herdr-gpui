use crate::terminal::*;
use gpui::*;
use herdr_client::protocol::{CellData, FrameData};
use std::collections::HashMap;

const CACHE_LIMIT: usize = 4096;

#[derive(Default)]
pub(crate) struct TerminalPainter {
    config: Option<Font>,
    // Resolved foreground includes reverse, dim and hidden; only bold/italic
    // affect shaping. Decorations remain at exact cell-grid coordinates.
    lines: HashMap<(u32, u16), HashMap<String, ShapedLine>>,
    entries: usize,
    cell_width: Option<f32>,
    #[cfg(feature = "integration-test")]
    pub uncached: bool,
}

fn style(cell: &CellData) -> (u32, u16) {
    (cell_colors(cell).0, cell.modifier & 5)
}

fn background_spans(row: &[CellData]) -> impl Iterator<Item = (usize, usize, u32)> + '_ {
    let mut start = 0;
    std::iter::from_fn(move || {
        let color = cell_colors(row.get(start)?).1;
        let mut end = start + 1;
        while end < row.len() && cell_colors(&row[end]).1 == color {
            end += 1;
        }
        let span = (start, end, color);
        start = end;
        Some(span)
    })
}

impl TerminalPainter {
    #[cfg(feature = "integration-test")]
    pub fn reset_cache(&mut self) {
        self.config = None;
        self.lines.clear();
        self.entries = 0;
        self.cell_width = None;
    }

    #[cfg(feature = "integration-test")]
    pub fn verify_native_cache(&self, window: &Window) -> Result<usize, String> {
        let Some(base) = &self.config else {
            return Err("missing font config".into());
        };
        for ((color, flags), lines) in &self.lines {
            let mut font = base.clone();
            if flags & 1 != 0 {
                font.weight = FontWeight::BOLD;
            }
            if flags & 4 != 0 {
                font.style = FontStyle::Italic;
            }
            for (symbol, cached) in lines {
                let fresh = window.text_system().shape_line(
                    symbol.clone().into(),
                    px(FONT_SIZE),
                    &[TextRun {
                        len: symbol.len(),
                        font: font.clone(),
                        color: rgb(*color).into(),
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    }],
                    None,
                );
                // Includes native glyph IDs/positions, font IDs, metrics and colors.
                if format!("{fresh:?}") != format!("{cached:?}") {
                    return Err(format!("cached glyph/style mismatch: {symbol:?}"));
                }
            }
        }
        Ok(self.entries)
    }
    fn configure(&mut self, font: &Font) {
        if self.config.as_ref() != Some(font) {
            self.lines.clear();
            self.entries = 0;
            self.cell_width = None;
            self.config = Some(font.clone());
        }
    }

    pub fn cell_width(&mut self, font: &Font, window: &Window, cx: &mut App) -> f32 {
        self.configure(font);
        if let Some(width) = self.cell_width {
            return width;
        }
        let width = window
            .text_system()
            .shape_line(
                "M".into(),
                px(FONT_SIZE),
                &[TextRun {
                    len: 1,
                    font: font.clone(),
                    color: rgb(FOREGROUND).into(),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                }],
                None,
            )
            .width
            .to_f64() as f32;
        #[cfg(feature = "integration-test")]
        {
            cx.default_global::<crate::performance::Counts>()
                .metric_shapes += 1;
        }
        #[cfg(not(feature = "integration-test"))]
        let _ = cx;
        self.cell_width = Some(width);
        width
    }

    #[allow(clippy::too_many_arguments)]
    pub fn paint_frame(
        &mut self,
        frame: &FrameData,
        origin: Point<Pixels>,
        cell_width: f32,
        font: &Font,
        window: &mut Window,
        cx: &mut App,
    ) {
        if frame.width == 0 {
            return;
        }
        self.configure(font);
        let cached = true;
        #[cfg(feature = "integration-test")]
        let cached = cached && !self.uncached;
        #[cfg(feature = "integration-test")]
        let mut counts = crate::performance::Counts::default();
        // Backgrounds precede all glyphs, including wide graphemes' skip cells.
        for (y, row) in frame.cells.chunks(usize::from(frame.width)).enumerate() {
            let mut paint = |start: usize, end: usize, color| {
                window.paint_quad(fill(
                    Bounds::new(
                        origin + point(px(start as f32 * cell_width), px(y as f32 * CELL_HEIGHT)),
                        size(px((end - start) as f32 * cell_width), px(CELL_HEIGHT)),
                    ),
                    rgb(color),
                ));
                #[cfg(feature = "integration-test")]
                {
                    counts.quads += 1;
                }
            };
            if cached {
                for (start, end, color) in background_spans(row) {
                    paint(start, end, color);
                }
            } else {
                for (x, cell) in row.iter().enumerate() {
                    paint(x, x + 1, cell_colors(cell).1);
                }
            }
        }
        for (index, cell) in frame.cells.iter().enumerate() {
            if cell.skip || cell.symbol.is_empty() || cell.symbol == " " {
                continue;
            }
            let key = style(cell);
            let mut overflow = HashMap::new();
            let lines = if self.entries < CACHE_LIMIT || self.lines.contains_key(&key) {
                self.lines.entry(key).or_default()
            } else {
                &mut overflow
            };
            let existing = cached.then(|| lines.get(cell.symbol.as_str())).flatten();
            let newly_shaped;
            let shaped = if let Some(line) = existing {
                line
            } else {
                let mut font = font.clone();
                if key.1 & 1 != 0 {
                    font.weight = FontWeight::BOLD;
                }
                if key.1 & 4 != 0 {
                    font.style = FontStyle::Italic;
                }
                #[cfg(feature = "integration-test")]
                {
                    counts.shapes += 1;
                }
                newly_shaped = window.text_system().shape_line(
                    cell.symbol.clone().into(),
                    px(FONT_SIZE),
                    &[TextRun {
                        len: cell.symbol.len(),
                        font,
                        color: rgb(key.0).into(),
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    }],
                    None,
                );
                if cached && self.entries < CACHE_LIMIT {
                    self.entries += 1;
                    lines.entry(cell.symbol.clone()).or_insert(newly_shaped)
                } else {
                    &newly_shaped
                }
            };
            let position = origin
                + point(
                    px((index % usize::from(frame.width)) as f32 * cell_width),
                    px((index / usize::from(frame.width)) as f32 * CELL_HEIGHT),
                );
            let result = shaped.paint(position, px(CELL_HEIGHT), window, cx);
            #[cfg(not(feature = "integration-test"))]
            let _ = result;
            #[cfg(feature = "integration-test")]
            {
                counts.glyphs += shaped.runs.iter().map(|r| r.glyphs.len()).sum::<usize>();
                counts.paint_errors += usize::from(result.is_err());
            }
            for (bit, y) in [(3, CELL_HEIGHT - 2.), (8, CELL_HEIGHT / 2.)] {
                if cell.modifier & (1 << bit) != 0 {
                    window.paint_quad(fill(
                        Bounds::new(
                            position + point(px(0.), px(y)),
                            size(px(cell_width), px(1.)),
                        ),
                        rgb(key.0),
                    ));
                    #[cfg(feature = "integration-test")]
                    {
                        counts.decorations += 1;
                    }
                }
            }
        }
        if let Some(cursor) = frame
            .cursor
            .as_ref()
            .filter(|c| c.visible && c.x < frame.width && c.y < frame.height)
        {
            let position = origin
                + point(
                    px(cursor.x as f32 * cell_width),
                    px(cursor.y as f32 * CELL_HEIGHT),
                );
            let (offset, dimensions) = match cursor.shape {
                3 | 4 => (
                    point(px(0.), px(CELL_HEIGHT - 2.)),
                    size(px(cell_width), px(2.)),
                ),
                5 | 6 => (point(px(0.), px(0.)), size(px(2.), px(CELL_HEIGHT))),
                _ => (point(px(0.), px(0.)), size(px(cell_width), px(CELL_HEIGHT))),
            };
            window.paint_quad(fill(
                Bounds::new(position + offset, dimensions),
                rgba(0xd8dee980),
            ));
            #[cfg(feature = "integration-test")]
            {
                counts.decorations += 1;
            }
        }
        #[cfg(feature = "integration-test")]
        {
            let total = cx.default_global::<crate::performance::Counts>();
            total.shapes += counts.shapes;
            total.quads += counts.quads;
            total.glyphs += counts.glyphs;
            total.decorations += counts.decorations;
            total.paint_errors += counts.paint_errors;
            total.paints += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    fn cell(symbol: &str) -> CellData {
        CellData {
            symbol: symbol.into(),
            fg: 0,
            bg: 0,
            modifier: 0,
            skip: false,
            hyperlink: None,
        }
    }

    #[test]
    fn spans_cover_skip_cells_and_resolved_colors_without_crossing_rows() {
        let mut row = vec![cell("\u{754c}"), cell(""), cell("x"), cell("x")];
        row[1].skip = true;
        row[2].fg = 0x02123456;
        row[2].modifier = 64;
        row[3].bg = 0x02123456;
        assert_eq!(
            background_spans(&row).collect::<Vec<_>>(),
            vec![(0, 2, BACKGROUND), (2, 4, 0x123456)]
        );
        assert_eq!(background_spans(&[]).count(), 0);
        for cells in row.chunks(2) {
            let expanded: Vec<_> = background_spans(cells)
                .flat_map(|(a, b, color)| (a..b).map(move |_| color))
                .collect();
            assert_eq!(
                expanded,
                cells.iter().map(|c| cell_colors(c).1).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn cache_style_includes_resolved_color_and_font_but_not_grid_decorations() {
        let base = cell("e\u{301}");
        let mut changed = base.clone();
        changed.modifier = 8 | 256;
        changed.hyperlink = Some(1);
        assert_eq!(style(&base), style(&changed));
        for modifier in [1, 4, 2, 64, 128] {
            changed.modifier = modifier;
            assert_ne!(style(&base), style(&changed));
        }
        changed.modifier = 0;
        changed.bg = 0x02abcdef;
        assert_eq!(style(&base), style(&changed));
        changed.fg = 0x02123456;
        assert_ne!(style(&base), style(&changed));
    }

    #[gpui::test]
    fn cache_reuses_cells_invalidates_fonts_and_bounds_storage(cx: &mut TestAppContext) {
        let (_, cx) = cx.add_window_view(|_, _| Empty);
        let painter = std::rc::Rc::new(std::cell::RefCell::new(TerminalPainter::default()));
        let frame = FrameData {
            width: 5,
            height: 1,
            cells: vec![
                cell("x"),
                cell("x"),
                cell("e\u{301}"),
                cell("\u{754c}"),
                CellData {
                    skip: true,
                    ..cell("")
                },
            ],
            cursor: None,
            hyperlinks: vec![],
            graphics: vec![],
        };
        let mut draw = |frame: FrameData, font: Font| {
            let painter = painter.clone();
            cx.draw(Point::default(), size(px(800.), px(600.)), |_, _| {
                canvas(
                    |_, _, _| (),
                    move |bounds, _, window, cx| {
                        painter.borrow_mut().paint_frame(
                            &frame,
                            bounds.origin,
                            8.4,
                            &font,
                            window,
                            cx,
                        );
                    },
                )
                .size_full()
            });
        };
        draw(frame.clone(), font("Menlo"));
        assert_eq!(painter.borrow().entries, 3);
        draw(frame.clone(), font("Menlo"));
        assert_eq!(painter.borrow().entries, 3);
        let mut changed = frame.clone();
        changed.cells[0].fg = 0x02123456;
        changed.cells[1].modifier = 1 | 4;
        draw(changed, font("Menlo"));
        assert_eq!(painter.borrow().entries, 5);
        draw(frame, font("Courier"));
        assert_eq!(painter.borrow().entries, 3, "new font discards old glyphs");
        let many = FrameData {
            width: 100,
            height: 50,
            cells: (0..5000)
                .map(|i| CellData {
                    fg: 0x02000000 | i,
                    ..cell("x")
                })
                .collect(),
            cursor: None,
            hyperlinks: vec![],
            graphics: vec![],
        };
        draw(many, font("Menlo"));
        assert_eq!(painter.borrow().entries, CACHE_LIMIT);
        assert_eq!(painter.borrow().lines.len(), CACHE_LIMIT);
    }
}
