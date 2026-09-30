<h1 align="center">Sippy</h1>

<p align="center">
  <img src="assets/logo/sippy-logo.png" alt="Sippy logo" width="200" />
</p>

<p align="center">
  <img src="assets/sippy/sippy-tilted.png" alt="Sippy, tilted" width="96" title="Ready" />
  <img src="assets/sippy/sippy-on-call.png" alt="Sippy in a call" width="96" title="On a call" />
  <img src="assets/sippy/sippy-dnd.png" alt="Sippy in do-not-disturb mode" width="96" title="Do not disturb" />
  <img src="assets/sippy/sippy-not-registered.png" alt="Sippy without SIP registration" width="96" title="Not registered" />
</p>

<p align="center"><sub>ready · on a call · do not disturb · not registered</sub></p>

Sippy is a SIP softphone for the Linux desktop, written in Rust with GPUI and built for Omarchy on Hyprland. The app starts its own baresip process and controls it over D-Bus.

## Showcase

The screenshots show made-up demo data.

[![Phone view with dial pad and sidebar navigation](docs/screenshots/desktop-phone.png)](docs/screenshots/desktop-phone.png)

In narrow windows the navigation moves to the top:

| Contacts | Incoming call |
| --- | --- |
| [![Contact list in the compact layout](docs/screenshots/compact-contacts.png)](docs/screenshots/compact-contacts.png) | [![Incoming demo call in the compact layout](docs/screenshots/compact-incoming.png)](docs/screenshots/compact-incoming.png) |

[All views in both window sizes](docs/screenshots/README.md) · [Screenshots as ZIP](docs/demo-screenshots.zip)

## Requirements

- Rust 1.88 or newer and Cargo
- a graphics driver with Vulkan support
- baresip with `ctrl_dbus`, PipeWire and `wpctl`
- `notify-send` and a session D-Bus
- for GPUI, the development libraries for Wayland, X11 and xkbcommon

`Cargo.lock` pins the tested dependencies, including GPUI 0.2.2.

## Build and install

```sh
make build
./target/release/sippy
```

`make install` builds the release binary and installs it with a desktop entry and app icon under `~/.local`. `PREFIX` changes that path. `make update` does the same. A running instance keeps running and picks up the new build on its next start.

### Upgrade from gosiptea-client

Use `make update` with the same `PREFIX` as before. After installing Sippy, the install script removes the old `bin/gosiptea-client`, `share/applications/gosiptea-client.desktop` and `share/icons/hicolor/512x512/apps/gosiptea-client.png` under that prefix. Start `sippy` instead of `gosiptea-client`.

The baresip configuration directory (default `~/.baresip`) and the files `gosiptea-settings.json` and `gosiptea-call-history.json` remain unchanged and compatible; no migration is needed. The storage lock `.gosiptea.lock` and D-Bus startup lock `com.github.GoSipTea.OwnedProcess` also keep their names for compatibility.

For development, `cargo build --locked` builds into `target/debug`. That build is not installed.

## Running

The app reads its configuration from `~/.baresip`. If another process already owns `com.github.Baresip` on the D-Bus, it does not start. It never stops or changes a baresip service it did not start.

To try it without a real account, use a temporary directory and a private D-Bus:

```sh
test_dir=$(mktemp -d)
dbus-run-session -- ./target/release/sippy --config-dir "$test_dir"
```

Don't enter real credentials there. baresip may create sample contacts in it. Delete the directory yourself afterwards.

The following options accept one or two leading dashes:

- `--config-dir PATH`
- `--baresip PATH`
- `--country-code CODE`, default `49`
- `--baresip-log PATH`
- `--sip-trace`, only together with `--baresip-log`

Without `--baresip-log`, baresip logs nothing. Logs and SIP traces can contain phone numbers and credentials.

## Usage

Wide windows show the navigation as a sidebar, narrow ones at the top. The question mark icon at its end, or `F1`, opens a list of all keyboard shortcuts. `Ctrl+1` to `Ctrl+5` open Phone, Contacts, Account, History and Settings. `Ctrl+Tab` and `Ctrl+Shift+Tab` go forward and back. `Ctrl+F` jumps to the contact search. These shortcuts also work in text fields. Everything else uses the mouse. Only the Phone view takes keyboard input without a click first.

- Phone: The dial pad follows the Android phone app. Typed characters go to the dial field, even after Esc. The green button dials, DND sits to its left, and backspace to its right deletes the last character. The volume control sits below.
- Call screen: Every incoming or outgoing call switches the app to Phone. Incoming calls pause MPRIS players, show a desktop notification and raise the window. They show "Decline" on the left and "Answer" on the right. During a call there are Mute, Keypad, Hold, DND, hang up and the volume control. The open keypad sends clicked and typed digits, `*` and `#` as DTMF right away. Mute and Keypad are disabled while the call is on hold. In the other views a banner leads back to the call.
- Contacts: A click selects an entry. The icons on the right call or delete it. "Add" opens the form.
- Account: Fill in the fields, toggle "TLS and SRTP" and save with "Save account". An empty password keeps the stored one.
- History: A click selects a call, "Dial" calls again.
- Settings: Language, theme and audio devices. Changes apply at once and are saved. Theme and volume are described below.
- Quit: Close the window. During a call the app asks first.

In text fields, Tab moves to the next field and Esc leaves the field. Enter dials in the dial field and saves in the address field of a new contact.

### Theme

"Dark" and "Light" are fixed Monokai Pro palettes. "Omarchy" takes its colors from `~/.local/state/omarchy/current/theme/colors.toml`, or from `~/.config/omarchy/current/theme/` on older Omarchy releases. The button only appears if that file exists at start or "Omarchy" is already saved. The app rereads the file every second, so switching with `omarchy-theme-set` shows up without a restart. A missing color comes from Light if the file sets `mode = "light"` or has a light background, and from Dark otherwise. Without the file, the app uses Dark.

### Audio and volume

Settings lists the devices for Output, Input and Ringtone. A click selects a device. A separate ringtone device takes effect only after a restart.

A click on the volume control sets the system volume of the selected output device in 5 % steps, with or without a call. With "System default" it controls the current default device. Changes made with the system control show up within about a second. The volume applies to the whole device, so it also changes other programs on it. With "Same as output" it also affects the ringtone. It doesn't control a separate ringtone device or the microphone.

### Limits

The app handles one account and one call at a time. The history keeps at most 200 calls in `gosiptea-call-history.json`. There are no global shortcuts or notification actions.

## Architecture

- `src/domain.rs`: call states, registration, contact matching, normalization and text limits, free of side effects.
- `src/storage.rs`: compatible baresip files, restrictive permissions, atomic writes and first-run setup.
- `src/platform.rs`: the app's own baresip process, a verified D-Bus connection, PipeWire, MPRIS, notifications and Hyprland focus.
- `src/session.rs`: a worker handles actions and events one at a time. The UI gets copied snapshots without the stored password.
- `src/ui.rs`: GPUI workspace with navigation, content area, status bar and quit dialog.
- `src/settings.rs`: language, translations and color palettes.
- `src/input.rs`: length-limited Unicode input with IME, selection, clipboard and password masking.
- `src/assets.rs`: UI icons and the app logo embedded in the binary.

The app and this README use `assets/logo/sippy-logo.png`. The desktop launcher uses its 512 × 512 derivative, `assets/logo/sippy-icon.png`, installed as `share/icons/hicolor/512x512/apps/sippy.png` under the installation prefix. When replacing the logo, regenerate the desktop icon and the [demo screenshots](docs/screenshots/README.md).

## Checks

```sh
make check
```

`make check` runs the formatter check, the tests and Clippy. The tests use temporary files, simulated SIP events and private D-Buses, never a real SIP account. The GPUI interaction tests run on GPUI's test platform.

`make smoke` opens the release build as a real window on the running Hyprland desktop, with a temporary configuration and a private D-Bus. It checks the baresip owner, the missing SIP account and that the app's own baresip process exits cleanly. `make check` doesn't run it.

A real call through a SIP server to another party remains a manual test.

`scripts/capture-demo.py` recreates the demo screenshots, see [docs/screenshots/README.md](docs/screenshots/README.md).

## References and license

- [oma.sip](https://github.com/Vinicius-Galleti/oma.sip) by Vinicius Galleti inspired the app. `src/config.tmpl` is based on its baresip configuration template.
- [Zed](https://github.com/zed-industries/zed), mainly its workspace layout and GPUI entity model.
- [GPUI](https://gpui.rs/) and the input example from version 0.2.2.
- [Material Symbols](https://github.com/google/material-design-icons) for the icons.

The MIT license is in `LICENSE`. It keeps the copyright notice of oma.sip for the configuration template. `src/input.rs` contains the attribution and the Apache 2.0 license of the GPUI example it is based on. The icons in `assets/icons` are also under Apache 2.0, with the license in `assets/icons/LICENSE`.
