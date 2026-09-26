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
pub fn palette(cx: &App) -> Palette {
    match preferences(cx).theme {
        Theme::Dark => Palette {
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
        },
        Theme::Light => Palette {
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
        },
    }
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
        "Ctrl+1-6 or Ctrl+Tab switch views" => "Strg+1-6 oder Strg+Tab wechselt die Ansicht",
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
}
