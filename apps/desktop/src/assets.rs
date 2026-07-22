//! Embedded SVG asset source for the app's vector icons.
//!
//! GPUI resolves `svg().path("…")` through the [`AssetSource`] registered on the
//! `Application` (via `with_assets`). The app otherwise renders icons as monochrome
//! unicode glyphs; a handful of marks (the Docker whale, the MCP glyph) read far
//! cleaner as real vector logos, so they ship embedded here and are tinted at render
//! by the element's `text_color` (GPUI paints the SVG as a single-colour mask, so the
//! logos adopt our status colours automatically).
//!
//! Icons live under `apps/desktop/assets/icons/` and are baked into the binary with
//! `include_bytes!`, so they travel with the bare `cargo run` build (no bundle needed).

use std::borrow::Cow;

use anyhow::Result;
use gpui::{AssetSource, SharedString};

/// The app's embedded asset source. Serves the `icons/*.svg` marks referenced by the
/// panels; unknown paths return `None` (GPUI then draws nothing rather than erroring).
pub struct Assets;

/// One embedded icon: its GPUI asset path and its bytes.
const ICONS: &[(&str, &[u8])] = &[
    (
        "icons/docker.svg",
        include_bytes!("../assets/icons/docker.svg"),
    ),
    ("icons/mcp.svg", include_bytes!("../assets/icons/mcp.svg")),
    // Lucide (ISC) marks for the Docker sub-sections.
    (
        "icons/layers.svg",
        include_bytes!("../assets/icons/layers.svg"),
    ),
    (
        "icons/container.svg",
        include_bytes!("../assets/icons/container.svg"),
    ),
    (
        "icons/network.svg",
        include_bytes!("../assets/icons/network.svg"),
    ),
    (
        "icons/hard-drive.svg",
        include_bytes!("../assets/icons/hard-drive.svg"),
    ),
    (
        "icons/boxes.svg",
        include_bytes!("../assets/icons/boxes.svg"),
    ),
];

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(ICONS
            .iter()
            .find(|(p, _)| *p == path)
            .map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(ICONS
            .iter()
            .filter(|(p, _)| p.starts_with(path))
            .map(|(p, _)| SharedString::from(*p))
            .collect())
    }
}
