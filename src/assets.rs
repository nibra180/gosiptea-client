use std::borrow::Cow;

use anyhow::Result;
use gpui::{AssetSource, SharedString};

/// Material Symbols Rounded, filled, Apache 2.0. See assets/icons/LICENSE.
const ICONS: [(&str, &[u8]); 10] = [
    (
        "icons/backspace.svg",
        include_bytes!("../assets/icons/backspace.svg"),
    ),
    ("icons/call.svg", include_bytes!("../assets/icons/call.svg")),
    (
        "icons/call_end.svg",
        include_bytes!("../assets/icons/call_end.svg"),
    ),
    (
        "icons/delete.svg",
        include_bytes!("../assets/icons/delete.svg"),
    ),
    (
        "icons/dialpad.svg",
        include_bytes!("../assets/icons/dialpad.svg"),
    ),
    (
        "icons/do_not_disturb_on.svg",
        include_bytes!("../assets/icons/do_not_disturb_on.svg"),
    ),
    ("icons/help.svg", include_bytes!("../assets/icons/help.svg")),
    (
        "icons/keyboard_hide.svg",
        include_bytes!("../assets/icons/keyboard_hide.svg"),
    ),
    (
        "icons/mic_off.svg",
        include_bytes!("../assets/icons/mic_off.svg"),
    ),
    (
        "icons/pause.svg",
        include_bytes!("../assets/icons/pause.svg"),
    ),
];

const LOGOS: [(&str, &[u8]); 1] = [(
    "logo/sippy-logo.png",
    include_bytes!("../assets/logo/sippy-logo.png"),
)];

/// Images are compiled in, so the binary needs no files at runtime.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(ICONS
            .iter()
            .chain(LOGOS.iter())
            .find(|(name, _)| *name == path)
            .map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(ICONS
            .iter()
            .chain(LOGOS.iter())
            .filter(|(name, _)| name.starts_with(path))
            .map(|(name, _)| SharedString::from(*name))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_logo_loads_as_png() {
        let name = "logo/sippy-logo.png";
        assert_eq!(
            Assets.list("logo/").unwrap(),
            vec![SharedString::from(name)]
        );
        let bytes = Assets.load(name).unwrap().unwrap();
        assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert!(Assets.load("logo/gosiptea-icon.svg").unwrap().is_none());
    }

    #[test]
    fn every_icon_loads_as_svg() {
        assert_eq!(Assets.list("icons/").unwrap().len(), ICONS.len());
        for (name, _) in ICONS {
            let bytes = Assets.load(name).unwrap().unwrap();
            assert!(bytes.starts_with(b"<svg"), "{name}");
        }
        assert!(Assets.load("icons/missing.svg").unwrap().is_none());
    }
}
