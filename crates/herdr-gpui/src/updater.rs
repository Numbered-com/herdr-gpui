//! Optional native updates. Call explicitly from normal GPUI startup, never from
//! CLI/test startup, and keep the returned controller alive for the app session.

#[cfg(target_os = "macos")]
use objc2::{
    MainThreadOnly, extern_class, msg_send,
    rc::{Allocated, Retained},
    runtime::{AnyObject, NSObject},
    sel,
};
#[cfg(target_os = "macos")]
use objc2_foundation::{NSBundle, NSDictionary, NSError, NSNumber, NSString, ns_string};

// SAFETY: These are NSObject subclasses in the pinned Sparkle 2.10.0 headers.
// Both APIs require the main thread. Class lookup is fallible and happens only
// after loading the bundled framework; we never use ClassType::class().
#[cfg(target_os = "macos")]
extern_class!(
    #[allow(unsafe_code)]
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    struct SPUStandardUpdaterController;
);

#[cfg(target_os = "macos")]
extern_class!(
    #[allow(unsafe_code)]
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    struct SPUUpdater;
);

#[cfg(target_os = "macos")]
extern_class!(
    #[allow(unsafe_code)]
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    struct SPUStandardUserDriver;
);

#[cfg(target_os = "macos")]
extern_class!(
    #[allow(unsafe_code)]
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    struct SUAppcastItem;
);

#[cfg(target_os = "macos")]
extern_class!(
    #[allow(unsafe_code)]
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    struct SPUUserUpdateState;
);

#[cfg(target_os = "macos")]
fn load_framework(app: &NSBundle) -> Result<Retained<NSBundle>, String> {
    let directory = app
        .privateFrameworksPath()
        .ok_or("App has no private frameworks directory")?;
    let path = NSString::from_str(&format!("{directory}/Sparkle.framework"));
    let framework = NSBundle::bundleWithPath(&path)
        .ok_or_else(|| format!("App is missing a valid framework at {path}"))?;
    // SAFETY: Callers check the main thread and app identity first. Load only
    // the fixed-name packaged framework, without search-path fallback or unload.
    #[allow(unsafe_code)]
    unsafe { framework.loadAndReturnError() }
        .map_err(|error| format!("Could not load {path}: {}", error.localizedDescription()))?;
    Ok(framework)
}

/// Independent QA UI, with no updater, feed, download, or installation session.
/// Drop the previous preview before calling `show` again from a menu action.
pub(super) struct UpdatePreview {
    #[cfg(target_os = "macos")]
    driver: Retained<SPUStandardUserDriver>,
    // Release native objects before the bundle; never unload framework code.
    #[cfg(target_os = "macos")]
    _framework: Retained<NSBundle>,
}

impl UpdatePreview {
    #[cfg(target_os = "macos")]
    #[allow(unsafe_code)]
    pub(super) fn show() -> Result<Self, String> {
        let _main_thread = objc2::MainThreadMarker::new()
            .ok_or("Sparkle preview must be shown on the main thread")?;
        let app = NSBundle::mainBundle();
        if std::path::Path::new(&app.bundlePath().to_string())
            .extension()
            .is_none_or(|extension| extension != "app")
            || app.bundleIdentifier().as_deref() != Some(ns_string!("so.pen.herdr-gpui"))
        {
            return Err("Sparkle preview requires the Herdr.app bundle (so.pen.herdr-gpui); build/run just bundle-updater-preview".into());
        }
        let automatic = app.objectForInfoDictionaryKey(ns_string!("SUAllowsAutomaticUpdates"));
        if !preview_preferences_disabled(automatic.as_deref()) {
            return Err("Sparkle preview requires SUAllowsAutomaticUpdates to be an NSNumber false in Info.plist so it cannot change updater preferences".into());
        }
        let framework = load_framework(&app)
            .map_err(|error| format!("{error}; build/run just bundle-updater-preview"))?;
        let version =
            framework.objectForInfoDictionaryKey(ns_string!("CFBundleShortVersionString"));
        if version
            .as_deref()
            .and_then(|value| value.downcast_ref::<NSString>())
            != Some(ns_string!("2.10.0"))
        {
            return Err("Sparkle QA preview requires exactly Sparkle 2.10.0 for its private state initializer; build/run just bundle-updater-preview".into());
        }
        let driver_class = framework
            .classNamed(ns_string!("SPUStandardUserDriver"))
            .ok_or("Sparkle.framework is missing SPUStandardUserDriver")?;
        let item_class = framework
            .classNamed(ns_string!("SUAppcastItem"))
            .ok_or("Sparkle.framework is missing SUAppcastItem")?;
        let state_class = framework
            .classNamed(ns_string!("SPUUserUpdateState"))
            .ok_or("Sparkle.framework is missing SPUUserUpdateState")?;
        for (class, selectors) in [
            (
                driver_class,
                &[
                    sel!(initWithHostBundle:delegate:),
                    sel!(showUpdateFoundWithAppcastItem:state:reply:),
                    sel!(dismissUpdateInstallation),
                ][..],
            ),
            (item_class, &[sel!(initWithDictionary:failureReason:)][..]),
            (state_class, &[sel!(initWithStage:userInitiated:)][..]),
        ] {
            for &selector in selectors {
                if class.instance_method(selector).is_none() {
                    return Err(format!(
                        "Incompatible Sparkle.framework: {class} lacks {selector}"
                    ));
                }
            }
        }

        let dictionary = preview_dictionary();
        let mut failure: Option<Retained<NSString>> = None;
        // SAFETY: Sparkle 2.10.0 SUAppcastItem.h declares this deprecated public
        // initializer with an autoreleasing NSString**, NOT NSError**. objc2
        // retains the out value and adopts the alloc/init +1 result.
        let item: Option<Retained<SUAppcastItem>> = unsafe {
            let allocated: Allocated<SUAppcastItem> = msg_send![item_class, alloc];
            msg_send![allocated, initWithDictionary: &*dictionary, failureReason: &mut failure]
        };
        let item = item.ok_or_else(|| {
            format!(
                "Could not create QA appcast item: {}",
                failure.map_or_else(
                    || "no failure reason supplied".into(),
                    |value| value.to_string()
                )
            )
        })?;
        // SAFETY: QA ONLY private API, pinned above to Sparkle 2.10.0:
        // Sparkle/SPUUserUpdateState+Private.h, initWithStage:userInitiated:.
        // SPUUserUpdateStage is NSInteger; 0 means NotDownloaded. Re-audit this
        // path when upgrading Sparkle, including SUUpdateAlert/driver behavior.
        let state: Option<Retained<SPUUserUpdateState>> = unsafe {
            let allocated: Allocated<SPUUserUpdateState> = msg_send![state_class, alloc];
            msg_send![allocated, initWithStage: 0_isize, userInitiated: true]
        };
        let state = state.ok_or("Sparkle preview state initialization returned nil")?;
        // SAFETY: Public SPUStandardUserDriver.h initializer accepts nil delegate.
        // All native objects remain main-thread-only; no SPUUpdater is created.
        let driver: Option<Retained<SPUStandardUserDriver>> = unsafe {
            let allocated: Allocated<SPUStandardUserDriver> = msg_send![driver_class, alloc];
            msg_send![allocated, initWithHostBundle: &*app, delegate: None::<&AnyObject>]
        };
        let preview = Self {
            driver: driver.ok_or("Sparkle preview driver initialization returned nil")?,
            _framework: framework,
        };
        // SUUpdateAlert closes before replying for Install/Skip/Later and window
        // close. The standard driver then releases its alert. No choice reaches
        // an updater, persists skipped versions, or starts a download. Only the
        // native window's normal placement autosave may write defaults.
        let reply: block2::RcBlock<dyn Fn(isize)> = block2::RcBlock::new(|_| {});
        // SAFETY: Public SPUUserDriver signature uses NSInteger choice and a
        // copied block; the alert retains item/state and copies the reply.
        unsafe {
            let _: () = msg_send![&*preview.driver,
                showUpdateFoundWithAppcastItem: &*item, state: &*state, reply: &*reply];
        }
        Ok(preview)
    }

    #[cfg(not(target_os = "macos"))]
    pub(super) fn show() -> Result<Self, String> {
        Err("Native Sparkle update preview is available only on macOS".into())
    }
}

impl Drop for UpdatePreview {
    fn drop(&mut self) {
        #[cfg(target_os = "macos")]
        // SAFETY: Validated public selector on a live main-thread-only driver.
        // Idempotent even after the user closed the alert; no updater is involved.
        #[allow(unsafe_code)]
        unsafe {
            let _: () = msg_send![&*self.driver, dismissUpdateInstallation];
        }
    }
}

#[cfg(target_os = "macos")]
fn preview_preferences_disabled(value: Option<&AnyObject>) -> bool {
    value
        .and_then(|value| value.downcast_ref::<NSNumber>())
        .is_some_and(|value| !value.as_bool())
}

#[cfg(target_os = "macos")]
fn preview_dictionary() -> Retained<NSDictionary<NSString, AnyObject>> {
    let enclosure = NSDictionary::from_slices(
        &[ns_string!("url"), ns_string!("length")],
        &[
            ns_string!("https://example.invalid/Herdr-QA-Preview.zip"),
            ns_string!("1"),
        ],
    );
    let description = NSDictionary::from_slices(
        &[ns_string!("content"), ns_string!("format")],
        &[
            ns_string!(
                "QA preview only. Install, Skip, and Later only dismiss this window. No update is checked, downloaded, installed, skipped, or scheduled. Updater preferences are unchanged."
            ),
            ns_string!("plain-text"),
        ],
    );
    // Fixed, bounded input with no links or remote release notes. The invalid
    // enclosure URL supplies the normal Install button, never a Learn More URL.
    NSDictionary::from_slices(
        &[
            ns_string!("sparkle:version"),
            ns_string!("sparkle:shortVersionString"),
            ns_string!("enclosure"),
            ns_string!("description"),
        ],
        &[
            ns_string!("99991231.99").as_ref(),
            ns_string!("99991231.99 (QA preview)").as_ref(),
            enclosure.as_ref(),
            description.as_ref(),
        ],
    )
}

pub(super) struct Updater {
    #[cfg(target_os = "macos")]
    controller: Retained<SPUStandardUpdaterController>,
    #[cfg(target_os = "macos")]
    updater: Retained<SPUUpdater>,
    // Drop objects before releasing the bundle. NSBundle release does not unload
    // executable code, and we must never call unload (including on error paths).
    #[cfg(target_os = "macos")]
    _framework: Retained<NSBundle>,
}

impl Updater {
    /// Local executables and bundles without update configuration opt out.
    #[cfg(target_os = "macos")]
    #[allow(unsafe_code)]
    pub(super) fn start() -> Result<Option<Self>, String> {
        let _main_thread = objc2::MainThreadMarker::new()
            .ok_or("Sparkle must be initialized on the main thread")?;
        let app = NSBundle::mainBundle();
        if std::path::Path::new(&app.bundlePath().to_string())
            .extension()
            .is_none_or(|extension| extension != "app")
            || app
                .bundleIdentifier()
                .as_deref()
                .map(NSString::to_string)
                .as_deref()
                != Some("so.pen.herdr-gpui")
        {
            return Ok(None);
        }
        for key in ["SUFeedURL", "SUPublicEDKey"] {
            let Some(value) = app.objectForInfoDictionaryKey(&NSString::from_str(key)) else {
                return Ok(None);
            };
            let value = value
                .downcast_ref::<NSString>()
                .ok_or_else(|| format!("Sparkle {key} must be a string in the app's Info.plist"))?;
            if value.to_string().trim().is_empty() {
                return Ok(None);
            }
        }

        let framework = load_framework(&app)?;
        let controller_class = framework
            .classNamed(&NSString::from_str("SPUStandardUpdaterController"))
            .ok_or("Sparkle.framework is missing SPUStandardUpdaterController")?;
        let updater_class = framework
            .classNamed(&NSString::from_str("SPUUpdater"))
            .ok_or("Sparkle.framework is missing SPUUpdater")?;
        for (class, selectors) in [
            (
                controller_class,
                &[
                    sel!(initWithStartingUpdater:updaterDelegate:userDriverDelegate:),
                    sel!(updater),
                    sel!(checkForUpdates:),
                ][..],
            ),
            (
                updater_class,
                &[sel!(startUpdater:), sel!(canCheckForUpdates)][..],
            ),
        ] {
            for &selector in selectors {
                if class.instance_method(selector).is_none() {
                    return Err(format!(
                        "Incompatible Sparkle.framework: {class} lacks {selector}"
                    ));
                }
            }
        }

        // SAFETY: Signatures/ownership match Sparkle 2.10.0's public headers.
        // alloc/init transfer +1 ownership into Retained; the updater getter is
        // +0 and msg_send retains it. nil delegates are explicitly supported.
        // MainThreadOnly keeps these objects (and their destruction) on this thread.
        let (controller, updater) = unsafe {
            let allocated: Allocated<SPUStandardUpdaterController> =
                msg_send![controller_class, alloc];
            let controller: Option<Retained<SPUStandardUpdaterController>> = msg_send![
                allocated, initWithStartingUpdater: false,
                updaterDelegate: None::<&AnyObject>, userDriverDelegate: None::<&AnyObject>
            ];
            let controller = controller.ok_or("Sparkle controller initialization returned nil")?;
            let updater: Option<Retained<SPUUpdater>> = msg_send![&*controller, updater];
            (
                controller,
                updater.ok_or("Sparkle controller returned no updater")?,
            )
        };
        // SAFETY: startUpdater: takes an autoreleasing NSError** and returns BOOL.
        // objc2 handles retaining the error written back through this argument.
        // Start directly to return configuration errors rather than the standard
        // controller's delayed modal alert. Sparkle schedules work asynchronously.
        let mut error: Option<Retained<NSError>> = None;
        let started: bool = unsafe { msg_send![&*updater, startUpdater: &mut error] };
        if !started {
            return Err(format!(
                "Could not start Sparkle: {}",
                error.map_or_else(
                    || "no NSError was supplied".to_owned(),
                    |error| error.localizedDescription().to_string(),
                )
            ));
        }
        Ok(Some(Self {
            controller,
            updater,
            _framework: framework,
        }))
    }

    #[cfg(not(target_os = "macos"))]
    pub(super) fn start() -> Result<Option<Self>, String> {
        Ok(None)
    }

    pub(super) fn can_check_for_updates(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            // SAFETY: Validated Sparkle getter returning BOOL; MainThreadOnly
            // and retained ownership ensure a live receiver on the main thread.
            #[allow(unsafe_code)]
            unsafe {
                msg_send![&*self.updater, canCheckForUpdates]
            }
        }
        #[cfg(not(target_os = "macos"))]
        false
    }

    pub(super) fn check_for_updates(&self) {
        #[cfg(target_os = "macos")]
        if self.can_check_for_updates() {
            // SAFETY: The standard controller accepts a nil sender. The retained
            // MainThreadOnly receiver is live, and readiness was just checked.
            #[allow(unsafe_code)]
            unsafe {
                let _: () = msg_send![&*self.controller, checkForUpdates: None::<&AnyObject>];
            }
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn preview_requires_explicit_numeric_false() {
        assert!(!preview_preferences_disabled(None));
        assert!(!preview_preferences_disabled(Some(ns_string!("false"))));
        assert!(!preview_preferences_disabled(Some(&NSNumber::new_bool(
            true
        ))));
        assert!(preview_preferences_disabled(Some(&NSNumber::new_bool(
            false
        ))));
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn preview_dictionary_is_bounded_and_has_only_inline_plain_text() {
        let dictionary = preview_dictionary();
        assert_eq!(dictionary.count(), 4);
        for (key, expected) in [
            ("sparkle:version", "99991231.99"),
            ("sparkle:shortVersionString", "99991231.99 (QA preview)"),
        ] {
            let value = dictionary.objectForKey(&NSString::from_str(key)).unwrap();
            assert_eq!(
                value.downcast_ref::<NSString>().unwrap().to_string(),
                expected
            );
        }
        let enclosure = dictionary.objectForKey(ns_string!("enclosure")).unwrap();
        let enclosure = enclosure
            .downcast_ref::<NSDictionary<AnyObject, AnyObject>>()
            .unwrap();
        assert_eq!(enclosure.count(), 2);
        let url = enclosure.objectForKey(ns_string!("url")).unwrap();
        assert_eq!(
            url.downcast_ref::<NSString>().unwrap(),
            ns_string!("https://example.invalid/Herdr-QA-Preview.zip")
        );
        let description = dictionary.objectForKey(ns_string!("description")).unwrap();
        let description = description
            .downcast_ref::<NSDictionary<AnyObject, AnyObject>>()
            .unwrap();
        assert_eq!(description.count(), 2);
        let format = description.objectForKey(ns_string!("format")).unwrap();
        assert_eq!(
            format.downcast_ref::<NSString>().unwrap(),
            ns_string!("plain-text")
        );
        let content = description.objectForKey(ns_string!("content")).unwrap();
        let content = content.downcast_ref::<NSString>().unwrap().to_string();
        assert!(content.contains("Install, Skip, and Later only dismiss"));
        assert!(content.len() < 512);
        assert!(!content.contains("https://"));
        for key in [
            "sparkle:releaseNotesLink",
            "sparkle:fullReleaseNotesLink",
            "link",
        ] {
            assert!(dictionary.objectForKey(&NSString::from_str(key)).is_none());
        }
    }
}
