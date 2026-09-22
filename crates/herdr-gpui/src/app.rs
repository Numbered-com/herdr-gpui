//! Process bootstrap and window opening. CLI parsing and the updater helper
//! run before GPUI starts, so an invalid option or `--build-info` exits without
//! ever creating a window.

use crate::{
    APP_VERSION, HerdrWindow, Quit, ShowLogs, WINDOW_TITLE, app_icon, bind_keys, cli, diagnostics,
    icons, log_window, menus, titlebar, updater,
};
#[cfg(feature = "integration-test")]
use crate::{performance, smoke};
use anyhow::Result;
use gpui::{prelude::*, *};
use herdr_client::ConnectTarget;

/// Opens one main window onto `target`. Every window is an independent client
/// of that daemon: its own connection, surface lease, and workspace focus.
pub(crate) fn open_window(
    target: ConnectTarget,
    updater: updater::Updater,
    cx: &mut App,
    #[cfg(feature = "integration-test")] fixture: bool,
) -> Result<WindowHandle<HerdrWindow>> {
    // Cascade rather than stack windows exactly, so a new one is visible at once.
    let existing = cx
        .windows()
        .iter()
        .filter(|handle| handle.downcast::<HerdrWindow>().is_some())
        .count();
    let step = px(28. * existing.min(6) as f32);
    let mut bounds = Bounds::centered(None, size(px(1200.), px(780.)), cx);
    bounds.origin += point(step, step);
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(640.), px(400.))),
            titlebar: Some(titlebar::options(WINDOW_TITLE)),
            app_id: Some("so.pen.herdr-gpui".into()),
            ..Default::default()
        },
        |window, cx| {
            cx.new(|cx| {
                let mut view = HerdrWindow::new(
                    target,
                    window,
                    cx,
                    #[cfg(feature = "integration-test")]
                    fixture,
                );
                view.updater = updater;
                view
            })
        },
    )
}

/// Opens another window from inside the focused window's own update.
pub(crate) fn open_additional_window(target: ConnectTarget, cx: &mut App) {
    cx.defer(move |cx| {
        let opened = open_window(
            target,
            updater::Updater::secondary(),
            cx,
            #[cfg(feature = "integration-test")]
            false,
        );
        match opened {
            Ok(handle) => {
                let _ = handle.update(cx, |view, window, _| {
                    view.sound = crate::sound::Service::new();
                    window.activate_window();
                });
            }
            Err(_) => tracing::error!("Unable to open an additional Herdr window"),
        }
    });
}

pub(crate) fn run() -> std::process::ExitCode {
    use cli::{LaunchMode, LaunchOptions};
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if let Some(exit) = updater::run_helper(&args) {
        return exit;
    }
    let LaunchOptions { target, mode } = match LaunchOptions::parse(args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!(
                "{error}\nUsage: herdr-gpui [--socket CLIENT_SOCKET | --session NAME [--dev]]"
            );
            return std::process::ExitCode::from(2);
        }
    };
    if mode == LaunchMode::BuildInfo {
        print!("{}", cli::build_info());
        return std::process::ExitCode::SUCCESS;
    }
    if mode == LaunchMode::Help {
        println!(
            "herdr-gpui [--socket CLIENT_SOCKET | --session NAME [--dev]]\nConnects to Local and saved SSH hosts; never installs remote software.\nStarts the local Herdr daemon if needed; never stops it.\nExplicit --socket and --dev targets are attach-only; --socket isolates the GUI to one existing daemon."
        );
        println!(
            "  --build-info        Print the executable's build identity without starting the GUI"
        );
        #[cfg(feature = "integration-test")]
        println!(
            "  --integration-test  Run native GUI checks (requires explicit --socket)\n  --sidebar-test      Run native sidebar fixtures without connecting to a daemon\n  --performance-test  Measure native dense-terminal hover/scroll without a daemon (macOS)"
        );
        return std::process::ExitCode::SUCCESS;
    }
    #[cfg(feature = "integration-test")]
    let integration_test = mode == LaunchMode::Integration;
    #[cfg(feature = "integration-test")]
    let sidebar_test = mode == LaunchMode::Sidebar;
    #[cfg(feature = "integration-test")]
    let performance_test = mode == LaunchMode::Performance;
    #[cfg(feature = "integration-test")]
    if mode != LaunchMode::Normal {
        smoke::EXIT_CODE.store(1, std::sync::atomic::Ordering::SeqCst);
    }
    let startup_failed = std::rc::Rc::new(std::cell::Cell::new(false));
    if let Err(error) = diagnostics::init() {
        eprintln!("Unable to initialize diagnostics: {error}");
        return std::process::ExitCode::FAILURE;
    }
    tracing::info!(
        version = APP_VERSION,
        os = std::env::consts::OS,
        arch = std::env::consts::ARCH,
        "GPUI client starting"
    );
    let failed = startup_failed.clone();
    Application::new().with_assets(icons::Icons).run(move |cx| {
        app_icon::install();
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.on_action(|_: &ShowLogs, cx| log_window::open(cx));
        bind_keys(cx);
        cx.set_menus(menus());
        cx.on_window_closed(move |cx| {
            if cx.windows().is_empty() {
                #[cfg(feature = "integration-test")]
                if performance_test {
                    std::process::exit(1);
                }
                cx.quit();
            }
        })
        .detach();
        // Native test modes and CLI invocations never start an updater worker.
        let updater = if mode == LaunchMode::Normal {
            updater::Updater::start()
        } else {
            updater::Updater::default()
        };
        let opened = open_window(
            target,
            updater,
            cx,
            #[cfg(feature = "integration-test")]
            {
                sidebar_test || performance_test
            },
        );
        match opened {
            Ok(_window) => {
                if mode == LaunchMode::Normal {
                    let _ = _window.update(cx, |view, _, _| {
                        view.sound = crate::sound::Service::new();
                    });
                }
                #[cfg(feature = "integration-test")]
                if performance_test {
                    performance::start(_window, cx);
                }
                #[cfg(feature = "integration-test")]
                if integration_test {
                    smoke::start(_window, cx);
                }
                #[cfg(feature = "integration-test")]
                if sidebar_test {
                    smoke::start_sidebar(_window, cx);
                }
            }
            Err(error) => {
                tracing::error!("Unable to open main window");
                eprintln!("Unable to open Herdr window: {error}");
                failed.set(true);
                #[cfg(feature = "integration-test")]
                if performance_test {
                    std::process::exit(1);
                }
                cx.quit();
            }
        }
        cx.activate(true);
    });
    if startup_failed.get() {
        std::process::ExitCode::FAILURE
    } else {
        std::process::ExitCode::SUCCESS
    }
}
