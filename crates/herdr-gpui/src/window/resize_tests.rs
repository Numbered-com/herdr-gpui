use super::HerdrWindow;
use herdr_client::ConnectTarget;

#[gpui::test]
fn resize_tracks_cell_metrics_and_retries_failed_options(cx: &mut gpui::TestAppContext) {
    let (view, cx) = cx.add_window_view(|window, cx| {
        HerdrWindow::new(
            ConnectTarget::Socket("/unused-resize-test.sock".into()),
            window,
            cx,
            true,
        )
    });
    view.update(cx, |view, _| {
        let client = herdr_client::connect(
            view.endpoints[view.selected_endpoint]
                .connection
                .target
                .clone(),
            view.options,
        )
        .unwrap_or_else(|error| panic!("cannot create test client: {error}"));
        client.handle.disconnect();
        view.endpoints[view.selected_endpoint].connection.handle = Some(client.handle);
        let queued = view.options;
        view.last_queued_options = Some(queued);
        view.resize();
        assert!(
            view.local_error.is_none(),
            "identical options are not resent"
        );
        view.options.cell_width_px += 1;
        view.resize();
        assert!(
            view.local_error.is_some(),
            "cell metrics alone trigger a send"
        );
        assert_eq!(view.last_queued_options, Some(queued));
        view.local_error = None;
        view.resize();
        assert!(view.local_error.is_some(), "failed options are retried");
    });
}
