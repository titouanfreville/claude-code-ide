//! Application / Dock / window icon.
//!
//! Each platform takes the icon by a different route, so there are two entry
//! points here:
//!
//! * macOS — GPUI exposes no app-icon API at our pinned revision, so the Dock
//!   icon for `cargo run` / RustRover launches is set at runtime via AppKit's
//!   `NSApplication setApplicationIconImage:` ([`set_app_icon`]). For a
//!   distributed bundle the `.icns` declared in `[package.metadata.bundle]`
//!   supplies the same icon; calling it is harmless there too.
//! * X11 — the window manager reads `_NET_WM_ICON` off the window, which GPUI
//!   fills from `WindowOptions.icon` ([`window_icon`]).
//! * Wayland — neither applies: the compositor resolves the icon from the
//!   `.desktop` file whose name matches the window's `app_id`, which is what
//!   `packaging/linux/` installs.
//!
//! The PNG is embedded in the binary, so every path works for a bare binary with
//! no bundle or install step.

/// The app icon, embedded so it ships with the bare `cargo run` binary.
const ICON_BYTES: &[u8] = include_bytes!("../../../icon.png");

/// Longest edge of the icon handed to the window manager. `_NET_WM_ICON` is an
/// uncompressed property (4 bytes per pixel, so the full 634×642 source would be
/// a 1.6 MB X request) and nothing renders it above ~128px anyway.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
const WINDOW_ICON_EDGE: u32 = 128;

/// The window icon for X11 — what the WM shows in Alt-Tab, the window list and
/// the taskbar. `None` on the platforms that take one of the other routes above;
/// GPUI ignores the field there regardless.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
pub fn window_icon() -> Option<std::sync::Arc<image::RgbaImage>> {
    let decoded = image::load_from_memory_with_format(ICON_BYTES, image::ImageFormat::Png)
        .inspect_err(|error| tracing::warn!(error = %error, "app icon failed to decode"))
        .ok()?;
    let scaled = decoded.resize(
        WINDOW_ICON_EDGE,
        WINDOW_ICON_EDGE,
        image::imageops::FilterType::Lanczos3,
    );
    Some(std::sync::Arc::new(scaled.into_rgba8()))
}

/// No window icon outside X11 (see the module docs).
#[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
pub fn window_icon() -> Option<std::sync::Arc<image::RgbaImage>> {
    None
}

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

/// No-op off macOS — the other platforms go through [`window_icon`] or the
/// installed `.desktop` entry.
#[cfg(not(target_os = "macos"))]
pub fn set_app_icon() {}
