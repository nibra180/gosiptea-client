# GoSipTea client

GoSipTea ist ein SIP-Softphone für den Linux-Desktop. Dieses Projekt portiert das Terminal-Programm GoSipTea nach Rust und GPUI, zugeschnitten auf Omarchy mit Hyprland. Die App startet einen eigenen baresip-Prozess und steuert ihn über D-Bus. Go-Quellbaum und Go-Binary braucht sie nicht.

## Showcase

Die Aufnahmen zeigen erfundene Demo-Daten.

[![Telefonansicht mit Wähltastenfeld und seitlicher Navigation](docs/screenshots/desktop-phone.png)](docs/screenshots/desktop-phone.png)

In schmalen Fenstern liegt die Navigation oben:

| Kontakte | Eingehender Anruf |
| --- | --- |
| [![Kontaktliste im kompakten Layout](docs/screenshots/compact-contacts.png)](docs/screenshots/compact-contacts.png) | [![Eingehender Demo-Anruf im kompakten Layout](docs/screenshots/compact-incoming.png)](docs/screenshots/compact-incoming.png) |

[Alle Ansichten in beiden Fenstergrößen](docs/screenshots/README.md) · [Screenshots als ZIP](docs/demo-screenshots.zip)

## Voraussetzungen

- Rust ab Version 1.88 und Cargo
- ein Vulkan-fähiger Grafiktreiber
- baresip mit `ctrl_dbus`, PipeWire und `wpctl`
- `notify-send` und ein Session-D-Bus
- für GPUI die Entwicklungsbibliotheken für Wayland, X11 und xkbcommon

`Cargo.lock` legt die geprüften Abhängigkeiten fest, darunter GPUI 0.2.2.

## Bauen und installieren

```sh
make build
./target/release/gosiptea-client
```

`make install` baut den Release-Build und legt Binary und Desktop-Eintrag unter `~/.local` ab. `PREFIX` ändert diesen Pfad. `make update` macht dasselbe. Eine laufende Instanz bleibt dabei unangetastet und nutzt den neuen Build ab dem nächsten Start. Eine vorhandene Go-Installation bleibt bestehen.

Für die Entwicklung baut `cargo build --locked` nach `target/debug`. Dieser Build wird nicht installiert.

## Starten

Die App liest ihre Konfiguration aus `~/.baresip`. Besitzt schon ein anderer Prozess `com.github.Baresip` auf dem D-Bus, startet sie nicht. Einen fremden baresip-Dienst stoppt oder verändert sie nie.

Zum Ausprobieren ohne echtes Konto dienen ein temporäres Verzeichnis und ein eigener D-Bus:

```sh
test_dir=$(mktemp -d)
dbus-run-session -- ./target/release/gosiptea-client --config-dir "$test_dir"
```

Dort keine echten Zugangsdaten eintragen. baresip legt darin eventuell Beispielkontakte an. Das Verzeichnis danach selbst löschen.

Die Optionen des Originals funktionieren mit einfachem und doppeltem Bindestrich:

- `--config-dir PATH`
- `--baresip PATH`
- `--country-code CODE`, standardmäßig `49`
- `--baresip-log PATH`
- `--sip-trace`, nur zusammen mit `--baresip-log`

Ohne `--baresip-log` schreibt baresip nichts mit. Logs und SIP-Traces können Rufnummern und Zugangsdaten enthalten.

## Bedienung

Breite Fenster zeigen die Navigation als Seitenleiste, schmale oben. `Ctrl+1` bis `Ctrl+5` öffnen Phone, Contacts, Account, History und Settings. `Ctrl+Tab` und `Ctrl+Shift+Tab` blättern vor und zurück. Diese Kürzel wirken auch in Textfeldern, alles andere läuft über die Maus. Nur die Phone-Ansicht nimmt Tastatureingaben ohne vorherigen Klick an.

- Phone: Das Wähltastenfeld folgt der Android-Telefon-App. Getippte Zeichen landen im Dial-Feld, auch nach Esc. Der grüne Button wählt, links davon schaltet DND, rechts löscht Backspace das letzte Zeichen. Darunter liegt der Lautstärkeregler.
- Anrufbildschirm: Bei jedem eingehenden oder ausgehenden Anruf wechselt die App auf Phone. Eingehende Anrufe pausieren MPRIS-Player, zeigen eine Desktop-Benachrichtigung und holen das Fenster nach vorn. Sie haben „Decline“ links und „Answer“ rechts. Im Gespräch gibt es Mute, Keypad, Hold, DND, Auflegen und den Lautstärkeregler. Das offene Keypad sendet angeklickte und getippte Ziffern, `*` und `#` sofort als DTMF. Während Hold sind Mute und Keypad gesperrt. In den anderen Ansichten führt ein Banner zurück zum Gespräch.
- Contacts: Ein Klick wählt einen Eintrag aus. Die Icons rechts rufen an oder löschen. „Add“ öffnet das Formular.
- Account: Felder ausfüllen, „TLS and SRTP“ umschalten, mit „Save account“ speichern. Ein leeres Passwort behält das gespeicherte.
- History: Ein Klick wählt einen Anruf aus, „Dial“ ruft erneut an.
- Settings: Sprache, Design und Audiogeräte. Änderungen gelten sofort und werden gespeichert. Details zu Design und Lautstärke stehen unten.
- Beenden: Fenster schließen. Während eines Gesprächs fragt die App vorher nach.

In Textfeldern springt Tab zum nächsten Feld, Esc verlässt das Feld. Enter wählt im Dial-Feld und speichert im Adressfeld eines neuen Kontakts.

### Design

„Dark“ und „Light“ sind feste Monokai-Pro-Paletten. „Omarchy“ übernimmt die Farben aus `~/.local/state/omarchy/current/theme/colors.toml`, bei älteren Omarchy-Versionen aus `~/.config/omarchy/current/theme/`. Den Button gibt es nur, wenn die Datei beim Start existiert oder „Omarchy“ schon gespeichert ist. Die App liest die Datei jede Sekunde neu, ein Wechsel mit `omarchy-theme-set` erscheint also ohne Neustart. Fehlt ein Farbwert, nimmt die App ihn aus Light, wenn die Datei `mode = "light"` setzt oder einen hellen Hintergrund hat, sonst aus Dark. Fehlt die Datei ganz, gilt Dark.

### Audio und Lautstärke

In Settings stehen die Listen für Output, Input und Ringtone. Ein Klick übernimmt das Gerät. Ein eigenes Klingelgerät wirkt wie im Original erst nach einem Neustart.

Der Lautstärkeregler ist die einzige Funktion, die das Original nicht hat. Ein Klick setzt die Systemlautstärke des gewählten Ausgabegeräts in 5-%-Schritten, auch ohne Anruf. Bei „System default“ gilt das aktuelle Standardgerät. Änderungen am Systemregler zeigt die App nach etwa einer Sekunde an. Die Lautstärke gilt für das ganze Gerät, also auch für andere Programme darauf. Bei „Same as output“ betrifft sie auch den Klingelton. Ein eigenes Klingelgerät und das Mikrofon regelt sie nicht.

### Grenzen

Die App verwaltet ein Konto und ein Gespräch gleichzeitig. Der Verlauf speichert höchstens 200 Anrufe in `gosiptea-call-history.json`. Globale Tastenkürzel, Aktionen in Benachrichtigungen und die Buchstaben-Kürzel der TUI gibt es nicht.

## Architektur

- `src/domain.rs`: Anrufzustände, Registrierung, Kontaktabgleich, Normalisierung und Textbegrenzung ohne Seiteneffekte.
- `src/storage.rs`: kompatible baresip-Dateien, restriktive Rechte, atomare Schreibvorgänge und Erstkonfiguration.
- `src/platform.rs`: eigener baresip-Prozess, verifizierte D-Bus-Verbindung, PipeWire, MPRIS, Benachrichtigungen und Hyprland-Fokus.
- `src/session.rs`: Ein Worker arbeitet Aktionen und Ereignisse nacheinander ab. Die Oberfläche bekommt kopierte Snapshots ohne gespeichertes Passwort.
- `src/ui.rs`: GPUI-Workspace mit Navigation, Inhaltsbereich, Statuszeile und Beenden-Dialog.
- `src/settings.rs`: Sprache, Übersetzungen und Farbpaletten.
- `src/input.rs`: begrenzte Unicode-Eingabe mit IME, Auswahl, Zwischenablage und Passwortmaskierung.
- `src/assets.rs`: ins Binary eingebettete Icons.

## Prüfungen

```sh
make check
```

`make check` prüft Formatierung, Tests und Clippy. Die Tests nutzen temporäre Dateien, simulierte SIP-Ereignisse und private D-Busse, nie ein echtes SIP-Konto. Die GPUI-Interaktionstests laufen auf der Testplattform von GPUI.

`make smoke` öffnet den Release-Build als echtes Fenster auf dem laufenden Hyprland-Desktop, mit temporärer Konfiguration und privatem D-Bus. Der Test prüft den baresip-Eigentümer, das fehlende SIP-Konto und ob der eigene baresip-Prozess sauber endet. `make check` führt ihn nicht aus.

Ein echter Anruf über SIP-Server und Gegenstelle bleibt ein manueller Test.

Die Demo-Screenshots erstellt `scripts/capture-demo.py` neu, Details stehen in [docs/screenshots/README.md](docs/screenshots/README.md).

## Referenzen und Lizenz

- Das ursprüngliche GoSipTea liegt unverändert im benachbarten Projekt.
- [Zed](https://github.com/zed-industries/zed), vor allem Workspace-Aufbau und GPUI-Entity-Modell.
- [GPUI](https://gpui.rs/) und das Eingabebeispiel der Version 0.2.2.
- [Material Symbols](https://github.com/google/material-design-icons) für die Icons.

Die MIT-Lizenz des Ursprungsprojekts steht in `LICENSE`. `src/input.rs` enthält den Herkunftshinweis und die Apache-2.0-Lizenz des verwendeten GPUI-Beispiels. Die Icons unter `assets/icons` stehen ebenfalls unter Apache 2.0, die Lizenz liegt in `assets/icons/LICENSE`.
