use gpui::{prelude::*, *};

pub(super) fn render(worktree: bool, branch: &str, pr: &str) -> Option<Div> {
    worktree.then(|| {
        div()
            .debug_selector(|| "worktree-banner".into())
            .flex()
            .flex_none()
            .items_center()
            .gap(px(10.))
            .px(px(12.))
            .w_full()
            .h(px(28.))
            .overflow_hidden()
            .bg(rgb(0xf6c453))
            .text_color(rgb(0x402b08))
            .text_size(px(12.))
            .child(
                div()
                    .flex_none()
                    .font_weight(FontWeight::BOLD)
                    .child("WORKTREE BUILD"),
            )
            .child(
                div()
                    .debug_selector(|| "worktree-branch".into())
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(branch.to_owned()),
            )
            .when(!pr.is_empty(), |row| {
                row.child(
                    div()
                        .debug_selector(|| "worktree-pr".into())
                        .flex_none()
                        .child(format!("PR #{pr}")),
                )
            })
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::render;
    use gpui::{Context, IntoElement, Render, TestAppContext, Window, div, prelude::*, px, size};

    struct Fixture {
        worktree: bool,
        pr: &'static str,
    }

    impl Render for Fixture {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .flex()
                .flex_col()
                .children(render(
                    self.worktree,
                    "worktree/a-very-long-branch-name-that-must-not-push-the-pr-out-of-the-window",
                    self.pr,
                ))
                .child(div().debug_selector(|| "body".into()).flex_1().min_h_0())
        }
    }

    #[gpui::test]
    fn banner_reserves_space_only_for_worktrees(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(|_, _| Fixture {
            worktree: false,
            pr: "",
        });
        for worktree in [false, true] {
            for pr in ["", "12345"] {
                for width in [360., 640., 1200.] {
                    view.update(cx, |view, cx| {
                        view.worktree = worktree;
                        view.pr = pr;
                        cx.notify();
                    });
                    cx.simulate_resize(size(px(width), px(400.)));
                    cx.run_until_parked();
                    cx.update(|window, cx| {
                        window.refresh();
                        let _ = window.draw(cx);
                    });
                    let body = cx.debug_bounds("body").unwrap();
                    assert_eq!(body.top(), px(if worktree { 28. } else { 0. }));
                    assert_eq!(body.bottom(), px(400.));
                    if worktree {
                        let banner = cx.debug_bounds("worktree-banner").unwrap();
                        let branch = cx.debug_bounds("worktree-branch").unwrap();
                        assert_eq!(banner.size, size(px(width), px(28.)));
                        assert!(branch.right() <= banner.right());
                        if !pr.is_empty() {
                            let pr = cx.debug_bounds("worktree-pr").unwrap();
                            assert!(pr.left() >= branch.right());
                            assert!(pr.right() <= banner.right());
                        } else {
                            assert!(cx.debug_bounds("worktree-pr").is_none());
                        }
                    } else {
                        assert!(cx.debug_bounds("worktree-banner").is_none());
                    }
                }
            }
        }
    }
}
