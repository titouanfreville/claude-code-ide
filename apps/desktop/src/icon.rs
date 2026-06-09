//! Application / Dock icon.
//!
//! GPUI exposes no macOS app-icon API at our pinned revision — `WindowOptions.icon`
//! is X11-only — so the macOS Dock icon for `cargo run` / RustRover launches is set
//! at runtime via AppKit's `NSApplication setApplicationIconImage:`. The PNG is
//! embedded in the binary, so this works for a bare binary with no `.app` bundle.
//! For a distributed bundle the `.icns` declared in `[package.metadata.bundle]`
//! supplies the same icon; calling this is harmless there too.
//!
//! Must be called on the main thread once the application is initialized.

/// The app icon, embedded so it ships with the bare `cargo run` binary.
#[cfg(target_os = "macos")]
const ICON_BYTES: &[u8] = include_bytes!("../../../icon.png");

/// Set the macOS Dock icon from the embedded PNG. No-op on other platforms, and a
/// silent no-op if called off the main thread or the bytes fail to decode.
#[cfg(target_os = "macos")]
pub fn set_app_icon() {
    use objc2::{AnyThread, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::NSData;

    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let data = NSData::with_bytes(ICON_BYTES);
    let Some(image) = NSImage::initWithData(NSImage::alloc(), &data) else {
        return;
    };
    // SAFETY: called on the main thread (asserted via `mtm`) with a valid NSImage.
    unsafe {
        NSApplication::sharedApplication(mtm).setApplicationIconImage(Some(&image));
    }
}

/// No-op on non-macOS platforms (X11 icons are handled via `WindowOptions.icon`).
#[cfg(not(target_os = "macos"))]
pub fn set_app_icon() {}
