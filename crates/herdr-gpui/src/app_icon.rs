//! Embedded development icon; app bundles use their native Info.plist icon.
#[cfg(any(target_os = "macos", test))]
const PNG: &[u8] = include_bytes!("../../../assets/icons/herdr-1024.png");

#[cfg(target_os = "macos")]
pub fn install() {
    use objc2::{AnyThread, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::{NSBundle, NSData, NSProcessInfo, NSString};

    let Some(main_thread) = MainThreadMarker::new() else {
        return;
    };
    NSProcessInfo::processInfo().setProcessName(&NSString::from_str("Herdr"));
    if NSBundle::mainBundle()
        .objectForInfoDictionaryKey(&NSString::from_str("CFBundleIconFile"))
        .is_some()
    {
        return;
    }
    let Some(image) = NSImage::initWithData(NSImage::alloc(), &NSData::with_bytes(PNG)) else {
        eprintln!("Unable to decode the embedded Herdr icon");
        return;
    };
    // SAFETY: GPUI has initialized AppKit, the main-thread marker gates access,
    // and the setter receives a live, non-null NSImage (never None).
    #[allow(unsafe_code)]
    unsafe {
        NSApplication::sharedApplication(main_thread).setApplicationIconImage(Some(&image));
    }
}

#[cfg(not(target_os = "macos"))]
pub fn install() {}

#[cfg(all(target_os = "macos", feature = "integration-test"))]
pub fn verify_native() -> Result<(), String> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;
    let mtm = MainThreadMarker::new().ok_or("icon check must run on the main thread")?;
    let image = NSApplication::sharedApplication(mtm)
        .applicationIconImage()
        .ok_or("NSApplication has no icon")?;
    let size = image.size();
    if !image.isValid() || size.width != 1024. || size.height != 1024. {
        return Err(format!("invalid native icon: {size:?}"));
    }
    eprintln!("ICON native PASS: valid NSApplication image, 1024x1024");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::PNG;

    #[test]
    fn embedded_icon_is_a_nonempty_1024_square_png() {
        assert!(PNG.len() > 33);
        assert_eq!(&PNG[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&PNG[12..16], b"IHDR");
        assert_eq!(&PNG[16..20], &1024_u32.to_be_bytes());
        assert_eq!(&PNG[20..24], &1024_u32.to_be_bytes());
    }
}
