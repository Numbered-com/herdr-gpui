//! Native opt-in smoke driver. No test platform, blocking waits, or direct client requests.
use super::*;
use std::{
    sync::atomic::{AtomicU8, Ordering},
    time::Instant,
};

pub static EXIT_CODE: AtomicU8 = AtomicU8::new(0);

#[derive(Clone, Copy, Debug, Default)]
pub struct InputProbe {
    pub actions: u64,
    pub keys: u64,
    pub text: u64,
}

const STEPS: &[&str] = &[
    "initial painted surface",
    "Cmd-T new tab",
    "Cmd-D right split",
    "Cmd-Shift-D below split",
    "previous tab",
    "next tab",
    "Cmd-N workspace",
    "workspace navigation",
    "return to full-width tab",
    "text commit + Enter output",
    "native resize",
    "reconnect persisted state",
    "input after reconnect",
];

pub fn start(handle: WindowHandle<HerdrWindow>, cx: &mut App) {
    let timer = cx.background_executor().clone();
    cx.spawn(async move |cx| {
        let mut step = 0;
        let mut since = Instant::now();
        let mut boot = String::new();
        let mut workspace = String::new();
        let mut first_tab = String::new();
        let mut second_tab = String::new();
        let mut split_pane = String::new();
        let mut old_size = ClientSurfaceSize { cols: 0, rows: 0 };
        let marker = format!("HERDR_GUI_{}_OK", std::process::id());
        let reconnected_marker = format!("{marker}_RECONNECTED");
        let mut frames = 0_u64;
        loop {
            timer.timer(Duration::from_millis(100)).await;
            let result = AnyWindowHandle::from(handle).update(cx, |root, window, cx| -> Result<bool, String> {
                let view = root.downcast::<HerdrWindow>().map_err(|_| "unexpected window root")?;
                if frames == 0 {
                    // Exercise the regression: no foreground app or pre-existing input focus.
                    cx.hide();
                    window.blur();
                }
                // on_next_frame runs BEFORE draw, and hidden windows may not receive it.
                // Build the real native window's dispatch tree and input handler synchronously,
                // without activating the app or relying on desktop/OS keyboard focus.
                window.focus(&view.read(cx).focus);
                window.refresh();
                window.draw(cx).clear();
                frames += 1;
                let focused = view.read(cx).focus.is_focused(window);
                let active = window.is_window_active();
                let actions_ready = window.is_action_available(&NewTab, cx);
                let probe = view.read(cx).input_probe;
                let (live, local_error, options, sent_size, bounds) = {
                    let view = view.read(cx);
                    (view.live.clone(), view.local_error.clone(), view.options, view.sent_size, view.bounds)
                };
                let diagnostic = || format!(
                    "step={step} ({}) elapsed={:?} frames={frames} focus={focused} actions_ready={actions_ready} active={} probe={probe:?} status={:.160} local_error={:.240} live.error={:.240} connected={} snapshot={:?} surface={:?} size={:?} sent_size={sent_size:?}",
                    STEPS[step], since.elapsed(), active, live.status,
                    local_error.as_deref().unwrap_or("none"), live.error.as_deref().unwrap_or("none"), live.connected,
                    live.snapshot.as_ref().map(|s| (s.revision, s.workspaces.len(), s.tabs.len())),
                    live.surface.as_ref().map(|s| (s.projection_revision, s.panes.len(), s.frame.width, s.frame.height)), options.surface_size
                );
                if local_error.is_some() || live.error.is_some() {
                    return Err(diagnostic());
                }
                if since.elapsed() > Duration::from_secs(20) {
                    return Err(format!("timeout: {}", diagnostic()));
                }
                if !focused || !actions_ready { return Ok(false); }
                let (Some(snapshot), Some(surface)) = (&live.snapshot, &live.surface) else { return Ok(false) };
                if !live.connected || snapshot.boot_id != surface.boot_id || snapshot.revision != surface.projection_revision {
                    return Ok(false);
                }
                surface.frame.validate().map_err(|e| format!("invalid frame: {e}; {}", diagnostic()))?;
                if let Some(error) = &snapshot.config_diagnostic {
                    return Err(format!("config diagnostic: {error:?}; {}", diagnostic()));
                }
                let focused_tab = snapshot.focused_tab_id.as_deref().unwrap_or_default();
                let focused_workspace = snapshot.focused_workspace_id.as_deref().unwrap_or_default();
                let key = |name: &str, window: &mut Window, cx: &mut App| -> Result<(), String> {
                    let before = view.read(cx).input_probe;
                    let key = Keystroke::parse(name).map_err(|e| e.to_string())?;
                    if !window.dispatch_keystroke(key, cx) { return Err(format!("unhandled keystroke {name}")); }
                    let after = view.read(cx).input_probe;
                    let delivered = if name.starts_with("cmd-") {
                        after.actions == before.actions + 1
                    } else {
                        after.keys == before.keys + 1
                    };
                    if !delivered { return Err(format!("keystroke {name} missed intended handler: before={before:?} after={after:?}; {}", diagnostic())); }
                    Ok(())
                };
                match step {
                    0 if !focused_tab.is_empty() && surface.panes.len() == 1 && bounds.size.width > px(0.) => {
                        boot = snapshot.boot_id.clone();
                        workspace = focused_workspace.into();
                        first_tab = focused_tab.into();
                        key("cmd-t", window, cx)?;
                    }
                    1 if snapshot.tabs.len() == 2 && focused_tab != first_tab && surface.panes.len() == 1 => {
                        second_tab = focused_tab.into();
                        split_pane = snapshot.focused_pane_id.clone().unwrap_or_default();
                        key("cmd-d", window, cx)?;
                    }
                    2 if surface.panes.len() == 2 => {
                        let old = surface.panes.iter().find(|p| p.pane_id == split_pane).ok_or("original split pane missing")?;
                        let new = surface.panes.iter().find(|p| Some(&p.pane_id) == snapshot.focused_pane_id.as_ref()).ok_or("focused split missing")?;
                        if new.rect.x <= old.rect.x || new.rect.y != old.rect.y { return Err(format!("right split geometry: {}", diagnostic())); }
                        split_pane = new.pane_id.clone();
                        key("cmd-shift-d", window, cx)?;
                    }
                    3 if surface.panes.len() == 3 => {
                        let old = surface.panes.iter().find(|p| p.pane_id == split_pane).ok_or("original split pane missing")?;
                        let new = surface.panes.iter().find(|p| Some(&p.pane_id) == snapshot.focused_pane_id.as_ref()).ok_or("focused split missing")?;
                        if new.rect.y <= old.rect.y || new.rect.x != old.rect.x { return Err(format!("down split geometry: {}", diagnostic())); }
                        window.dispatch_action(Box::new(PreviousTab), cx);
                    }
                    4 if focused_tab == first_tab && surface.panes.len() == 1 => {
                        window.dispatch_action(Box::new(NextTab), cx);
                    }
                    5 if focused_tab == second_tab && surface.panes.len() == 3 => {
                        key("cmd-n", window, cx)?;
                    }
                    6 if snapshot.workspaces.len() == 2 && focused_workspace != workspace && surface.panes.len() == 1 => {
                        view.update(cx, |view, cx| { view.navigate("workspace", &workspace, cx); window.focus(&view.focus); });
                    }
                    7 if focused_workspace == workspace && focused_tab == second_tab && surface.panes.len() == 3 => {
                        // Use the full-width tab so the exact output row cannot wrap in a split.
                        window.dispatch_action(Box::new(PreviousTab), cx);
                    }
                    8 if focused_tab == first_tab && surface.panes.len() == 1 => {
                        let command = format!("echo HERDR_GUI_{}\"_OK\"", std::process::id());
                        type_text(&command, &view, window, cx)?;
                        key("enter", window, cx)?;
                    }
                    9 if has_output(&surface.frame, &marker) => {
                        eprintln!("GUI shell output verified (not command echo): {marker}");
                        old_size = options.surface_size;
                        window.resize(size(px(1000.), px(650.)));
                    }
                    10 if options.surface_size != old_size && sent_size == Some(options.surface_size)
                        && surface.frame.width == options.surface_size.cols && surface.frame.height == options.surface_size.rows => {
                        eprintln!("GUI native resize verified: {:?} -> {:?}", old_size, options.surface_size);
                        view.update(cx, |view, cx| { view.reconnect(); window.focus(&view.focus); cx.notify(); });
                    }
                    11 if snapshot.boot_id == boot && snapshot.workspaces.len() == 2 && snapshot.tabs.len() == 3
                        && focused_workspace == workspace && focused_tab == first_tab && has_output(&surface.frame, &marker) => {
                        type_text(&format!("echo HERDR_GUI_{}\"_OK_RECONNECTED\"", std::process::id()), &view, window, cx)?;
                        key("enter", window, cx)?;
                    }
                    12 if has_output(&surface.frame, &reconnected_marker) => {
                        eprintln!("GUI input pipeline verified: frames={frames} focus={focused} active={active} probe={probe:?}");
                        eprintln!("GUI integration PASS: same boot={boot}, 2 workspaces / 3 tabs, persisted shell output after reconnect");
                        eprintln!("GUI fresh input after reconnect verified: {reconnected_marker}");
                        EXIT_CODE.store(0, Ordering::SeqCst);
                        cx.quit();
                        return Ok(true);
                    }
                    _ => return Ok(false),
                }
                eprintln!("GUI step {step} ({}) verified; waiting for {}", STEPS[step], STEPS[step + 1]);
                step += 1;
                since = Instant::now();
                Ok(false)
            });
            match result {
                Ok(Ok(true)) => break,
                Ok(Ok(false)) => {},
                error => {
                    EXIT_CODE.store(1, Ordering::SeqCst);
                    eprintln!("GUI integration FAIL: {error:?}");
                    let _ = cx.update(|cx| cx.quit());
                    break;
                }
            }
        }
    }).detach();
}

fn type_text(
    text: &str,
    view: &Entity<HerdrWindow>,
    window: &mut Window,
    cx: &mut App,
) -> Result<(), String> {
    for ch in text.chars() {
        let before = view.read(cx).input_probe.text;
        if !window.dispatch_keystroke(
            Keystroke {
                modifiers: Modifiers::default(),
                key: ch.to_string(),
                key_char: Some(ch.to_string()),
            },
            cx,
        ) {
            return Err(format!("unhandled text keystroke {ch:?}"));
        }
        if view.read(cx).input_probe.text != before + 1 {
            return Err(format!("text keystroke {ch:?} missed native input handler"));
        }
    }
    Ok(())
}

fn has_output(frame: &FrameData, marker: &str) -> bool {
    frame.width > 0
        && frame.cells.chunks(usize::from(frame.width)).any(|row| {
            row.iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
                .trim()
                == marker
        })
}
