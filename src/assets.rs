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

const LOGOS: [(&str, &[u8]); 5] = [
    ("sippy/logo.svg", include_bytes!("../assets/sippy/logo.svg")),
    (
        "sippy/ready.svg",
        include_bytes!("../assets/sippy/ready.svg"),
    ),
    (
        "sippy/on-call.svg",
        include_bytes!("../assets/sippy/on-call.svg"),
    ),
    ("sippy/dnd.svg", include_bytes!("../assets/sippy/dnd.svg")),
    (
        "sippy/not-registered.svg",
        include_bytes!("../assets/sippy/not-registered.svg"),
    ),
];

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
    fn every_sippy_loads_as_vector_svg() {
        assert_eq!(Assets.list("sippy/").unwrap().len(), LOGOS.len());
        for (name, _) in LOGOS {
            let bytes = Assets.load(name).unwrap().unwrap();
            let svg = std::str::from_utf8(&bytes).unwrap();
            assert!(svg.starts_with("<svg"), "{name}");
            assert!(svg.contains("viewBox="), "{name}");
            assert!(svg.contains("<path"), "{name}");
            assert!(!svg.contains("<image"), "{name}");
            assert!(!svg.contains("data:image"), "{name}");
        }
        assert!(Assets.load("sippy/missing.svg").unwrap().is_none());
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
