use std::{collections::HashMap, path::PathBuf};

use gpui::{App, Global};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    #[default]
    English,
    German,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    Dark,
    Light,
    Omarchy,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Preferences {
    pub language: Language,
    pub theme: Theme,
}

impl Global for Preferences {}

pub fn preferences(cx: &App) -> Preferences {
    cx.try_global::<Preferences>().copied().unwrap_or_default()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    pub background: u32,
    pub panel: u32,
    pub border: u32,
    pub text: u32,
    pub muted: u32,
    pub accent: u32,
    pub good: u32,
    pub warning: u32,
    pub error: u32,
    pub selection: u32,
    pub hover: u32,
    pub overlay: u32,
    pub selection_text: u32,
}

// RGB values copied from ~/.config/omarchy/themes/{monokai-pro,monokai-pro-light}/colors.toml.
// The light palette uses the Sun filter.
// Roles: panel=dark_background; border/hover=lighter_background; text=foreground;
// muted=light_foreground; good=green; warning=yellow; error=red;
// selection=selection_background; overlay=darker_background; selection_text=selection_foreground.
const DARK: Palette = Palette {
    background: 0x2d2a2e,
    panel: 0x221f22,
    border: 0x403e41,
    text: 0xfcfcfa,
    muted: 0xc1c0c0,
    accent: 0xff6188,
    good: 0xa9dc76,
    warning: 0xffd866,
    error: 0xff6188,
    selection: 0x403e41,
    hover: 0x403e41,
    overlay: 0x19181a,
    selection_text: 0xfcfcfa,
};

const LIGHT: Palette = Palette {
    background: 0xf8efe7,
    panel: 0xeee5de,
    border: 0xded5d0,
    text: 0x2c232e,
    muted: 0x72696d,
    accent: 0xce4770,
    good: 0x218871,
    warning: 0xb16803,
    error: 0xce4770,
    selection: 0xbeb5b3,
    hover: 0xded5d0,
    overlay: 0xd2c9c4,
    selection_text: 0x2c232e,
};

pub fn palette(cx: &App) -> Palette {
    match preferences(cx).theme {
        Theme::Dark => DARK,
        Theme::Light => LIGHT,
        Theme::Omarchy => cx
            .try_global::<OmarchyTheme>()
            .map_or(DARK, |theme| theme.palette),
    }
}

/// The active Omarchy theme, re-read from its colors.toml while selected.
pub struct OmarchyTheme {
    path: Option<PathBuf>,
    source: Option<String>,
    palette: Palette,
}

impl Global for OmarchyTheme {}

impl OmarchyTheme {
    pub fn new(path: Option<PathBuf>) -> Self {
        Self {
            path,
            source: None,
            palette: DARK,
        }
    }

    /// Omarchy keeps the current theme under ~/.local/state; older releases used ~/.config.
    pub fn default_path() -> Option<PathBuf> {
        let home = PathBuf::from(std::env::var_os("HOME").filter(|home| !home.is_empty())?);
        [".local/state/omarchy", ".config/omarchy"]
            .into_iter()
            .map(|dir| home.join(dir).join("current/theme/colors.toml"))
            .find(|path| path.is_file())
            .or_else(|| Some(home.join(".local/state/omarchy/current/theme/colors.toml")))
    }
}

/// Re-reads the Omarchy colors and returns whether the palette changed.
pub fn reload_omarchy_theme(cx: &mut App) -> bool {
    let Some(theme) = cx.try_global::<OmarchyTheme>() else {
        return false;
    };
    let source = theme
        .path
        .as_ref()
        .and_then(|path| std::fs::read_to_string(path).ok());
    if source == theme.source {
        return false;
    }
    let palette = source.as_deref().map_or(DARK, omarchy_palette);
    let theme = cx.global_mut::<OmarchyTheme>();
    let changed = palette != theme.palette;
    theme.source = source;
    theme.palette = palette;
    changed
}

// Follows the alias and fallback order of omarchy-theme-color. Missing keys fall
// back to the built-in palette that matches the theme mode.
pub fn omarchy_palette(source: &str) -> Palette {
    let mut colors = HashMap::new();
    let mut mode = None;
    for line in source.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().trim_matches(['"', '\'']);
        if key.is_empty() || key.starts_with('#') {
            continue;
        }
        let value = value.trim();
        let value = match value.strip_prefix(['"', '\'']) {
            Some(quoted) => quoted.split(['"', '\'']).next().unwrap_or_default(),
            None => value,
        };
        if key == "mode" || (key == "theme_type" && mode.is_none()) {
            mode = Some(value == "light");
        } else if let Some(color) = value
            .strip_prefix('#')
            .filter(|hex| hex.len() == 6)
            .and_then(|hex| u32::from_str_radix(hex, 16).ok())
        {
            colors.insert(key.to_owned(), color);
        }
    }
    let get = |keys: &[&str]| keys.iter().find_map(|key| colors.get(*key).copied());
    let background = get(&["background", "bg", "color0"]);
    let light = mode.unwrap_or_else(|| {
        background.is_some_and(|bg| (bg >> 16) + (bg >> 8 & 0xff) + (bg & 0xff) > 382)
    });
    let base = if light { LIGHT } else { DARK };
    let Some(background) = background else {
        return base;
    };
    let text = get(&["foreground", "fg", "color7"]).unwrap_or(base.text);
    let lighter = get(&["lighter_background", "lighter_bg", "color0"]).unwrap_or(background);
    let error = get(&["red", "color1"]).unwrap_or(base.error);
    Palette {
        background,
        panel: get(&["dark_background", "dark_bg"]).unwrap_or(mix(background, 0, 25)),
        border: lighter,
        text,
        muted: get(&["light_foreground", "light_fg", "color7"]).unwrap_or(text),
        accent: get(&["accent"]).unwrap_or(error),
        good: get(&["green", "color2"]).unwrap_or(base.good),
        warning: get(&["yellow", "color3"]).unwrap_or(base.warning),
        error,
        selection: get(&["selection_background", "selection", "color8"]).unwrap_or(background),
        hover: lighter,
        overlay: get(&["darker_background", "darker_bg"]).unwrap_or(mix(background, 0, 50)),
        selection_text: get(&[
            "selection_foreground",
            "bright_foreground",
            "bright_fg",
            "color15",
        ])
        .unwrap_or(text),
    }
}

fn mix(start: u32, end: u32, percent: u32) -> u32 {
    [16, 8, 0].into_iter().fold(0, |color, shift| {
        let (a, b) = ((start >> shift) & 0xff, (end >> shift) & 0xff);
        color | ((a * (100 - percent) + b * percent + 50) / 100) << shift
    })
}

pub fn tr<'a>(cx: &App, text: &'a str) -> &'a str {
    if preferences(cx).language == Language::English {
        return text;
    }
    match text {
        "Settings" => "Einstellungen",
        "Language" => "Sprache",
        "English" => "Englisch",
        "German" => "Deutsch",
        "Theme" => "Design",
        "Dark" => "Dunkel",
        "Light" => "Hell",
        "Changes are saved automatically." => "Änderungen werden automatisch gespeichert.",
        "Ctrl+1-5 or Ctrl+Tab switch views" => "Strg+1-5 oder Strg+Tab wechselt die Ansicht",
        "Phone" => "Telefon",
        "Contacts" => "Kontakte",
        "Audio" => "Audio",
        "Account" => "Konto",
        "History" => "Verlauf",
        "Number or SIP address" => "Nummer oder SIP-Adresse",
        "Search contacts" => "Kontakte suchen",
        "Display name" => "Anzeigename",
        "Extension" => "Nebenstelle",
        "Defaults to server" => "Standardmäßig wie Server",
        "Defaults to user" => "Standardmäßig wie Benutzer",
        "Leave blank to keep saved password" => {
            "Leer lassen, um das gespeicherte Passwort zu behalten"
        }
        "DND" => "Nicht stören",
        "Decline" => "Ablehnen",
        "Answer" => "Annehmen",
        "Mute" => "Stummschalten",
        "Keypad" => "Tastenfeld",
        "Hold" => "Halten",
        "Return to call" => "Zurück zum Anruf",
        "Add contact" => "Kontakt hinzufügen",
        "Name" => "Name",
        "SIP address" => "SIP-Adresse",
        "Enter saves from the address field. Esc cancels." => {
            "Im Adressfeld speichert Enter. Esc bricht ab."
        }
        "Save contact" => "Kontakt speichern",
        "Cancel" => "Abbrechen",
        "Search" => "Suchen",
        "Add" => "Hinzufügen",
        "Dial" => "Anrufen",
        "No matching contacts." => "Keine passenden Kontakte.",
        "Restart GoSipTea to ring on the selected ringtone output." => {
            "GoSipTea neu starten, um die gewählte Klingeltonausgabe zu verwenden."
        }
        "Output" => "Ausgabe",
        "Input" => "Eingabe",
        "Ringtone" => "Klingelton",
        "Output volume" => "Ausgabelautstärke",
        "Unavailable" => "Nicht verfügbar",
        "unavailable" => "nicht verfügbar",
        "Server" => "Server",
        "User" => "Benutzer",
        "Domain" => "Domain",
        "Login" => "Anmeldename",
        "Password" => "Passwort",
        "Saved password will be kept" => "Gespeichertes Passwort wird beibehalten",
        "[x] TLS and SRTP" => "[x] TLS und SRTP",
        "[ ] TLS and SRTP" => "[ ] TLS und SRTP",
        "Save account" => "Konto speichern",
        "Call history" => "Anrufverlauf",
        "No calls yet." => "Noch keine Anrufe.",
        "Error" => "Fehler",
        "Muted" => "Stummgeschaltet",
        "DND on" => "Nicht stören an",
        "DND off" => "Nicht stören aus",
        "A call is in progress. Quit anyway?" => "Ein Anruf läuft. Trotzdem beenden?",
        "Quit" => "Beenden",
        "Stay" => "Bleiben",
        "Incoming call" => "Eingehender Anruf",
        "Calling…" => "Anruf läuft…",
        "On hold" => "Gehalten",
        "Same as output" => "Wie Ausgabe",
        "System default" => "Systemstandard",
        " (current system default)" => " (aktueller Systemstandard)",
        "Incoming" => "Eingehend",
        "Calling" => "Anruf läuft",
        "Active" => "Aktiv",
        "Idle" => "Bereit",
        "Unknown peer" => "Unbekannter Teilnehmer",
        "Registered" | "registered" => "Registriert",
        "registering" => "Registrierung läuft",
        "unregistered" => "Nicht registriert",
        "registration failed" => "Registrierung fehlgeschlagen",
        "Registering" => "Registrierung läuft",
        "Failed" => "Fehlgeschlagen",
        "Offline" => "Offline",
        "Unknown" => "Unbekannt",
        "Missed" => "Verpasst",
        "Rejected" => "Abgelehnt",
        "Busy" => "Besetzt",
        "Canceled" => "Abgebrochen",
        "Not connected" => "Nicht verbunden",
        "Today" => "Heute",
        "Yesterday" => "Gestern",
        _ => text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn defaults_and_global_preferences(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            assert_eq!(preferences(cx), Preferences::default());
            assert_eq!(preferences(cx).language, Language::English);
            assert_eq!(preferences(cx).theme, Theme::Dark);
            assert_eq!(tr(cx, "Phone"), "Phone");
            let dark = palette(cx);
            assert_eq!(dark.background, 0x2d2a2e);
            assert_eq!(dark.muted, 0xc1c0c0);
            assert_eq!(dark.selection_text, 0xfcfcfa);
            cx.set_global(Preferences {
                language: Language::German,
                theme: Theme::Light,
            });
            assert_eq!(tr(cx, "Phone"), "Telefon");
            assert_eq!(
                tr(cx, "Leave blank to keep saved password"),
                "Leer lassen, um das gespeicherte Passwort zu behalten"
            );
            let unknown = String::from("sip:alice@example.com");
            assert_eq!(tr(cx, &unknown), unknown);
            let light = palette(cx);
            assert_eq!(light.background, 0xf8efe7);
            assert_eq!(light.panel, 0xeee5de);
            assert_eq!(light.border, 0xded5d0);
            assert_eq!(light.text, 0x2c232e);
            assert_eq!(light.muted, 0x72696d);
            assert_eq!(light.accent, 0xce4770);
            assert_eq!(light.good, 0x218871);
            assert_eq!(light.warning, 0xb16803);
            assert_eq!(light.error, 0xce4770);
            assert_eq!(light.selection, 0xbeb5b3);
            assert_eq!(light.hover, 0xded5d0);
            assert_eq!(light.overlay, 0xd2c9c4);
            assert_eq!(light.selection_text, 0x2c232e);
        });
    }

    #[test]
    fn omarchy_palette_maps_semantic_legacy_and_missing_colors() {
        let monokai = include_str!("../tests/fixtures/monokai-pro-colors.toml");
        let semantic = omarchy_palette(monokai);
        assert_eq!(semantic, DARK);

        let legacy = omarchy_palette(
            "color0 = '#101010'\ncolor1 = \"#aa0000\" # red\ncolor2 = '#00aa00'\n\
             color3 = '#aaaa00'\ncolor7 = '#dddddd'\ncolor8 = '#555555'\n",
        );
        assert_eq!(legacy.background, 0x101010);
        assert_eq!(legacy.panel, 0x0c0c0c);
        assert_eq!(legacy.overlay, 0x080808);
        assert_eq!(legacy.border, 0x101010);
        assert_eq!(legacy.text, 0xdddddd);
        assert_eq!(legacy.muted, 0xdddddd);
        assert_eq!(legacy.accent, 0xaa0000);
        assert_eq!(legacy.error, 0xaa0000);
        assert_eq!(legacy.good, 0x00aa00);
        assert_eq!(legacy.warning, 0xaaaa00);
        assert_eq!(legacy.selection, 0x555555);
        assert_eq!(legacy.selection_text, 0xdddddd);

        let bright = omarchy_palette("background = \"#fafafa\"\n");
        assert_eq!(bright.background, 0xfafafa);
        assert_eq!(bright.text, LIGHT.text);
        assert_eq!(bright.good, LIGHT.good);
        assert_eq!(omarchy_palette("mode = \"light\"\n"), LIGHT);
        assert_eq!(omarchy_palette(""), DARK);
        assert_eq!(omarchy_palette("background = \"#12345\"\nfoo"), DARK);
    }

    #[gpui::test]
    fn omarchy_theme_reloads_only_when_the_file_changes(cx: &mut gpui::TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("colors.toml");
        cx.update(|cx| {
            cx.set_global(Preferences {
                theme: Theme::Omarchy,
                ..Preferences::default()
            });
            assert_eq!(palette(cx), DARK);
            cx.set_global(OmarchyTheme::new(Some(path.clone())));
            assert!(!reload_omarchy_theme(cx));
            std::fs::write(&path, "mode = \"light\"\n").unwrap();
            assert!(reload_omarchy_theme(cx));
            assert_eq!(palette(cx), LIGHT);
            assert!(!reload_omarchy_theme(cx));
            std::fs::write(&path, "background = \"#123456\"\n").unwrap();
            assert!(reload_omarchy_theme(cx));
            assert_eq!(palette(cx).background, 0x123456);
            std::fs::remove_file(&path).unwrap();
            assert!(reload_omarchy_theme(cx));
            assert_eq!(palette(cx), DARK);
        });
    }
}
