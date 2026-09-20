use crate::{
    diagnostics::{self, Record},
    search_input::{Changed, SearchInput},
};
use gpui::{prelude::*, *};
use std::{sync::Arc, time::Duration};
use tracing::Level;

const LEVELS: [Level; 5] = [
    Level::TRACE,
    Level::DEBUG,
    Level::INFO,
    Level::WARN,
    Level::ERROR,
];

actions!(log_window, [Close, FocusSearch]);

#[derive(Default)]
struct LogWindowHandle(Option<WindowHandle<LogWindow>>);
impl Global for LogWindowHandle {}

pub(super) fn open(cx: &mut App) {
    // Global menu actions can run inside the existing window's update.
    cx.defer(open_deferred);
}

fn open_deferred(cx: &mut App) {
    if let Some(handle) = cx.default_global::<LogWindowHandle>().0
        && handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    {
        return;
    }
    let bounds = Bounds::centered(None, size(px(1100.), px(650.)), cx);
    match cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(620.), px(360.))),
            titlebar: Some(TitlebarOptions {
                title: Some("Herdr GPUI Logs".into()),
                ..Default::default()
            }),
            ..Default::default()
        },
        |window, cx| cx.new(|cx| LogWindow::new(window, cx)),
    ) {
        Ok(handle) => cx.set_global(LogWindowHandle(Some(handle))),
        Err(_) => tracing::error!("Unable to open log window"),
    }
}

struct LogWindow {
    focus: FocusHandle,
    search: Entity<SearchInput>,
    enabled: [bool; 5],
    rows: Vec<Arc<Record>>,
    retained: Vec<Arc<Record>>,
    generation: Option<u64>,
    dropped: u64,
    following: bool,
    scroll: UniformListScrollHandle,
    selected: Option<Arc<Record>>,
    status: String,
    exporting: bool,
    _search: Subscription,
    _poll: Task<()>,
}

fn filtered(records: Vec<Arc<Record>>, query: &str, enabled: [bool; 5]) -> Vec<Arc<Record>> {
    let query = query.to_lowercase();
    records
        .into_iter()
        .filter(|record| {
            LEVELS
                .iter()
                .zip(enabled)
                .any(|(level, enabled)| enabled && *level == record.level)
                && (query.is_empty() || record.line.to_lowercase().contains(&query))
        })
        .collect()
}

fn export_text(rows: &[Arc<Record>], dropped: u64) -> String {
    let mut text = format!(
        "Herdr GPUI {} | {} {}\nFiltered local client diagnostics; timestamps are UNIX seconds.\nEvicted/dropped records: {dropped}. Review before sharing.\n\n",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    for row in rows {
        text.push_str(&row.line);
        text.push('\n');
    }
    text
}

impl LogWindow {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.bind_keys([
            KeyBinding::new("cmd-w", Close, Some("LogWindow")),
            KeyBinding::new("cmd-f", FocusSearch, Some("LogWindow")),
        ]);
        let search = cx.new(SearchInput::new);
        search.update(cx, |input, cx| {
            input.set_placeholder("Search messages, targets, timings...", cx);
            window.focus(&input.focus);
        });
        let subscription = cx.subscribe(&search, |this, _, _: &Changed, cx| {
            this.generation = None;
            cx.notify();
        });
        let poll = cx.spawn(async move |this, cx| {
            loop {
                let request = this.update(cx, |this, cx| {
                    (this.generation.is_none()
                        || (this.following && this.generation != Some(diagnostics::generation())))
                    .then(|| {
                        (
                            this.search.read(cx).text().to_owned(),
                            this.enabled,
                            this.following,
                            (!this.following).then(|| {
                                (
                                    diagnostics::generation(),
                                    this.retained.clone(),
                                    this.dropped,
                                )
                            }),
                        )
                    })
                });
                let Ok(request) = request else { break };
                if let Some((query, enabled, following, frozen)) = request {
                    let filter_query = query.clone();
                    let snapshot = cx
                        .background_executor()
                        .spawn(async move {
                            frozen.or_else(diagnostics::snapshot).map(
                                |(generation, retained, dropped)| {
                                    (
                                        generation,
                                        filtered(retained.clone(), &filter_query, enabled),
                                        retained,
                                        dropped,
                                    )
                                },
                            )
                        })
                        .await;
                    if let Some((generation, rows, retained, dropped)) = snapshot
                        && this
                            .update(cx, |this, cx| {
                                if this.search.read(cx).text() != query
                                    || this.enabled != enabled
                                    || this.following != following
                                {
                                    return;
                                }
                                this.generation = Some(generation);
                                this.rows = rows;
                                this.retained = retained;
                                this.dropped = dropped;
                                if this.following && !this.rows.is_empty() {
                                    this.scroll.scroll_to_item(
                                        this.rows.len() - 1,
                                        ScrollStrategy::Bottom,
                                    );
                                }
                                cx.notify();
                            })
                            .is_err()
                    {
                        break;
                    }
                }
                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;
            }
        });
        Self {
            focus: cx.focus_handle(),
            search,
            enabled: [true; 5],
            rows: Vec::new(),
            retained: Vec::new(),
            generation: None,
            dropped: 0,
            following: true,
            scroll: UniformListScrollHandle::new(),
            selected: None,
            status: "Local only. Review logs before sharing.".into(),
            exporting: false,
            _search: subscription,
            _poll: poll,
        }
    }

    fn share(&mut self, save: bool, cx: &mut Context<Self>) {
        if self.exporting {
            return;
        }
        self.exporting = true;
        self.status = if save {
            "Choose an export destination..."
        } else {
            "Preparing clipboard..."
        }
        .into();
        let records = self.retained.clone();
        let query = self.search.read(cx).text().to_owned();
        let enabled = self.enabled;
        let dropped = self.dropped;
        let picker =
            save.then(|| cx.prompt_for_new_path(std::path::Path::new("."), Some("herdr-gpui.log")));
        cx.spawn(async move |this, cx| {
            let path = match picker {
                Some(picker) => match picker.await {
                    Ok(Ok(Some(path))) => Some(path),
                    result => {
                        let cancelled = matches!(result, Ok(Ok(None)));
                        let _ = this.update(cx, |this, cx| {
                            this.exporting = false;
                            this.status = if cancelled {
                                "Export cancelled."
                            } else {
                                "Unable to open save dialog."
                            }
                            .into();
                            cx.notify();
                        });
                        return;
                    }
                },
                None => None,
            };
            let result = cx
                .background_executor()
                .spawn(async move {
                    let text = export_text(&filtered(records, &query, enabled), dropped);
                    if let Some(path) = path {
                        std::fs::write(path, text).map(|()| None)
                    } else {
                        Ok(Some(text))
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.exporting = false;
                match result {
                    Ok(Some(text)) => {
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                        this.status = "Filtered logs copied. Review before sharing.".into();
                    }
                    Ok(None) => {
                        this.status = "Filtered logs exported. Review before sharing.".into()
                    }
                    Err(error) => {
                        tracing::warn!(kind = ?error.kind(), "Log export failed");
                        this.status = format!("Export failed: {}", error.kind());
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

fn button(id: &'static str, label: impl Into<SharedString>) -> Stateful<Div> {
    div()
        .id(id)
        .debug_selector(move || id.into())
        .px_2()
        .py_1()
        .rounded_sm()
        .bg(rgb(0x263344))
        .cursor_pointer()
        .hover(|style| style.bg(rgb(0x34475e)))
        .child(label.into())
}

impl Render for LogWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .key_context("LogWindow")
            .track_focus(&self.focus)
            .on_action(cx.listener(|_, _: &Close, window, _| window.remove_window()))
            .on_action(cx.listener(|this, _: &FocusSearch, window, cx| {
                window.focus(&this.search.read(cx).focus);
            }))
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(0x111820))
            .text_color(rgb(0xd9e2ed))
            .text_size(px(12.))
            .child(
                div()
                    .flex_none()
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(div().text_size(px(16.)).child("GPUI / Diagnostics"))
                    .child(self.search.clone())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .children(LEVELS.iter().enumerate().map(|(index, level)| {
                                button(
                                    match index {
                                        0 => "trace",
                                        1 => "debug",
                                        2 => "info",
                                        3 => "warn",
                                        _ => "error",
                                    },
                                    level.as_str(),
                                )
                                .when(!self.enabled[index], |el| el.text_color(rgb(0x6a7889)))
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.enabled[index] = !this.enabled[index];
                                        this.generation = None;
                                        cx.notify();
                                    },
                                ))
                            }))
                            .child(div().flex_1())
                            .child(
                                button(
                                    "follow",
                                    if self.following {
                                        "Pause"
                                    } else {
                                        "Resume tail"
                                    },
                                )
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.following = !this.following;
                                        this.generation = None;
                                        cx.notify();
                                    },
                                )),
                            )
                            .child(
                                button("copy", "Copy")
                                    .on_click(cx.listener(|this, _, _, cx| this.share(false, cx))),
                            )
                            .child(
                                button(
                                    "export",
                                    if self.exporting {
                                        "Working..."
                                    } else {
                                        "Export..."
                                    },
                                )
                                .on_click(cx.listener(|this, _, _, cx| this.share(true, cx))),
                            ),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .on_scroll_wheel(cx.listener(|this, _, _, cx| {
                        this.following = false;
                        cx.notify();
                    }))
                    .when(self.rows.is_empty(), |el| {
                        el.child(div().p_3().child("No matching logs."))
                    })
                    .when(!self.rows.is_empty(), |el| {
                        el.child(
                            uniform_list(
                                "logs",
                                self.rows.len(),
                                cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                                    range
                                        .map(|index| {
                                            let record = this.rows[index].clone();
                                            let color = match record.level {
                                                Level::ERROR => 0xff8794,
                                                Level::WARN => 0xefc77a,
                                                Level::INFO => 0xd9e2ed,
                                                _ => 0x91a5bb,
                                            };
                                            div()
                                                .id(index)
                                                .debug_selector(move || format!("log-row-{index}"))
                                                .h(px(22.))
                                                .px_3()
                                                .text_color(rgb(color))
                                                .font_family("monospace")
                                                .truncate()
                                                .child(record.line.clone())
                                                .cursor_pointer()
                                                .hover(|style| style.bg(rgb(0x263344)))
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.selected = Some(record.clone());
                                                    cx.notify();
                                                }))
                                        })
                                        .collect()
                                }),
                            )
                            .track_scroll(self.scroll.clone())
                            .size_full(),
                        )
                    }),
            )
            .when_some(self.selected.clone(), |el, record| {
                el.child(
                    div()
                        .id("log-detail")
                        .h(px(100.))
                        .overflow_y_scroll()
                        .p_3()
                        .bg(rgb(0x1c2734))
                        .child(record.line.clone()),
                )
            })
            .child(
                div()
                    .flex_none()
                    .p_3()
                    .border_t_1()
                    .border_color(rgb(0x263344))
                    .child(format!(
                        "{} shown | {} evicted/dropped | {} | {}",
                        self.rows.len(),
                        self.dropped,
                        if self.following { "LIVE" } else { "PAUSED" },
                        self.status
                    )),
            )
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    fn records() -> Vec<Arc<Record>> {
        [
            (Level::WARN, "WARN slow paint elapsed_ms=32"),
            (Level::INFO, "INFO slow transport elapsed_ms=8"),
            (Level::WARN, "WARN unrelated message"),
        ]
        .into_iter()
        .map(|(level, line)| {
            Arc::new(Record {
                level,
                line: line.into(),
            })
        })
        .collect()
    }

    #[gpui::test]
    fn narrow_layout_renders_search_and_virtualized_rows(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut view = LogWindow::new(window, cx);
            view.following = false;
            view.generation = Some(diagnostics::generation());
            view.retained = (0..5000)
                .map(|index| {
                    Arc::new(Record {
                        level: Level::INFO,
                        line: format!("INFO fixture row {index}"),
                    })
                })
                .collect();
            view.rows = view.retained.clone();
            view
        });
        cx.simulate_resize(size(px(620.), px(650.)));
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear());
        let search = cx.debug_bounds("theme-search").unwrap();
        assert!(search.size.width > px(500.));
        assert!(search.size.height > px(0.));
        assert!(search.left() >= px(0.) && search.right() <= px(620.));
        let first = cx.debug_bounds("log-row-0").unwrap();
        let tenth = cx.debug_bounds("log-row-10").unwrap();
        assert_eq!(first.size.height, px(22.));
        assert!(first.top() >= search.bottom());
        assert_eq!(tenth.top() - first.top(), px(220.));
        assert!(tenth.bottom() < px(650.));
        assert!(cx.debug_bounds("log-row-4999").is_none());
        for selector in [
            "trace", "debug", "info", "warn", "error", "follow", "copy", "export",
        ] {
            let bounds = cx.debug_bounds(selector).unwrap();
            assert!(
                bounds.left() >= px(0.) && bounds.right() <= px(620.),
                "{selector}: {bounds:?}"
            );
        }
        view.update(cx, |view, cx| {
            assert!(view.scroll.is_scrollable());
            view.scroll.scroll_to_item(4999, ScrollStrategy::Bottom);
            cx.notify();
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        let last = cx.debug_bounds("log-row-4999").unwrap();
        assert!(last.top() > search.bottom() && last.bottom() < px(650.));
        cx.simulate_input("fixture");
        view.read_with(cx, |view, cx| {
            assert_eq!(view.search.read(cx).text(), "fixture")
        });
    }

    #[gpui::test]
    fn paused_filter_changes_preserve_retained_snapshot(cx: &mut TestAppContext) {
        let retained = records();
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut view = LogWindow::new(window, cx);
            view.following = false;
            view.generation = Some(diagnostics::generation());
            view.retained = retained.clone();
            view.rows = retained.clone();
            view.dropped = 17;
            view
        });
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear());
        cx.simulate_input("SLOW");
        cx.executor().advance_clock(Duration::from_millis(250));
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(!view.following);
            assert_eq!(view.rows.len(), 2);
            assert!(Arc::ptr_eq(&view.rows[0], &retained[0]));
            assert!(Arc::ptr_eq(&view.rows[1], &retained[1]));
        });
        cx.update(|window, cx| window.draw(cx).clear());
        let info = cx.debug_bounds("info").unwrap();
        cx.simulate_click(info.center(), Modifiers::default());
        cx.executor().advance_clock(Duration::from_millis(250));
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(!view.following);
            assert!(!view.enabled[2]);
            assert_eq!(view.rows.len(), 1);
            assert!(Arc::ptr_eq(&view.rows[0], &retained[0]));
            assert_eq!(view.dropped, 17);
            assert_eq!(view.retained.len(), retained.len());
            assert!(
                view.retained
                    .iter()
                    .zip(&retained)
                    .all(|(a, b)| Arc::ptr_eq(a, b))
            );
        });
    }

    #[gpui::test]
    fn copy_uses_current_query_and_levels_before_rows_refresh(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut view = LogWindow::new(window, cx);
            view.following = false;
            view.generation = Some(diagnostics::generation());
            view.retained = records();
            // The visible rows deliberately omit the record the current filter wants.
            view.rows = vec![view.retained[2].clone()];
            view.dropped = 23;
            view
        });
        cx.run_until_parked();
        view.update(cx, |view, cx| {
            view.search
                .update(cx, |input, cx| input.set_text_selected("SLOW", cx));
            view.enabled = [false, false, false, true, false];
            assert_eq!(view.rows[0].line, "WARN unrelated message");
            view.share(false, cx);
            assert!(view.exporting);
        });
        cx.run_until_parked();
        cx.update(|_, cx| {
            let text = cx.read_from_clipboard().unwrap().text().unwrap();
            assert!(text.contains("Evicted/dropped records: 23"));
            assert_eq!(
                text.split_once("\n\n").unwrap().1,
                "WARN slow paint elapsed_ms=32\n"
            );
            assert!(!view.read(cx).exporting);
            assert_eq!(
                view.read(cx).status,
                "Filtered logs copied. Review before sharing."
            );
        });
    }

    #[gpui::test]
    fn shortcuts_focus_search_and_close_only_log_window(cx: &mut TestAppContext) {
        let other = cx.add_window(|_, cx| SearchInput::new(cx));
        let (view, cx) = cx.add_window_view(|window, cx| {
            crate::bind_keys(cx);
            LogWindow::new(window, cx)
        });
        cx.update(|window, cx| {
            window.focus(&view.read(cx).focus);
            window.draw(cx).clear();
            assert!(!view.read(cx).search.read(cx).focus.is_focused(window));
        });
        cx.simulate_keystrokes("cmd-f");
        cx.update(|window, cx| assert!(view.read(cx).search.read(cx).focus.is_focused(window)));
        cx.simulate_keystrokes("cmd-w");
        assert!(cx.windows() == vec![other.into()]);
    }

    #[gpui::test]
    fn log_window_is_singleton(cx: &mut TestAppContext) {
        cx.update(|cx| {
            open(cx);
            open(cx);
        });
        cx.update(|cx| {
            assert_eq!(cx.windows().len(), 1);
            let handle = cx.default_global::<LogWindowHandle>().0;
            if let Some(handle) = handle {
                assert!(handle.update(cx, |_, _, cx| open(cx)).is_ok());
            }
        });
        cx.update(|cx| assert_eq!(cx.windows().len(), 1));
    }
    #[test]
    fn search_levels_and_export_preserve_full_lines() {
        let records = vec![
            Arc::new(Record {
                level: Level::WARN,
                line: "WARN slow paint elapsed_ms=32".into(),
            }),
            Arc::new(Record {
                level: Level::TRACE,
                line: "TRACE connected".into(),
            }),
        ];
        let rows = filtered(records.clone(), "SLOW", [true; 5]);
        assert_eq!(rows.len(), 1);
        assert!(export_text(&rows, 7).contains("Evicted/dropped records: 7"));
        assert!(export_text(&rows, 7).ends_with("WARN slow paint elapsed_ms=32\n"));
        assert!(filtered(records, "", [false; 5]).is_empty());
    }
}
