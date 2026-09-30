# Prüfprotokoll

## Prüfung der Umbenennung zu Sippy

- `cargo fmt --all -- --check`: erfolgreich.
- `cargo test --locked --all-targets`: 180 Tests erfolgreich; zwei bewusst ignorierte Tests.
- `cargo clippy --locked --all-targets -- -D warnings`: erfolgreich.
- `cargo build --locked --release`: erfolgreich.
- Release-Starttest unter Hyprland mit privatem D-Bus: Fenster mit Titel `Sippy` und App-ID `sippy`, eigener baresip-Prozess und sauberes Beenden erfolgreich geprüft.
- Installation mit temporärem `PREFIX`: `sippy --help`, Desktop-Eintrag und Icon geprüft; alte Binary, Desktop-Datei und Icon entfernt. Keine Installation in das Benutzerprofil.
- Neue Regressionstests prüfen die Weiterverwendung bestehender Einstellungen, Historie und Sperrpfade sowie das Installations-Upgrade einschließlich fehlgeschlagenem Build.
- Alle 14 Demo-Screenshots und das ZIP mit dem neuen Branding erneut erstellt.

## Frühere Prüfungen

Die folgenden Ergebnisse stammen aus der Zeit vor der Umbenennung zu Sippy.

Geprüft auf dem vorhandenen Omarchy-System mit Rust und Cargo 1.98.1, GPUI 0.2.2 und Hyprland 0.56.2.

## Automatisierte Prüfungen

- `make check`: Formatierung, 154 erfolgreiche Rust-/GPUI-Tests und Clippy mit `-D warnings`.
- `make smoke`: echtes GPUI-Fenster unter Wayland, eigener baresip-Prozess und privater D-Bus. Der Test prüft den Besitzer-PID, eine Konfiguration ohne aktives SIP-Konto und das Beenden des Kindprozesses per SIGTERM an die Anwendung.
- `make install` mit temporärem `PREFIX`: Binary ausführbar, Hilfe aufrufbar und Desktop-Eintrag mit `desktop-file-validate` geprüft. Keine Installation in das Benutzerprofil.

Die Tests decken Registrierung, Wählen, Annehmen, Ablehnen, Auflegen, Stummschaltung, Halten, DTMF, DND, verspätete und fremde Ereignisse, Verlauf, Kontaktabgleich, Audiorouting und das Speichern des Kontos ab. GPUI-Tests führen echte Eingabeereignisse auf der Testplattform aus und bedienen Buttons, Listen und Regler per Mausklick. Ein eigener Test prüft, dass außer den Ansichtskürzeln mit Ctrl keine Taste außerhalb von Textfeldern etwas auslöst. Die einzige Ausnahme ist die Phone-Ansicht ohne Anruf, dort gehen Eingaben ins Dial-Feld. Das und die Ansichtskürzel decken weitere Tests ab. Private D-Bus-Tests prüfen Prozessbesitz, Signalabsender, Besitzerwechsel, frühe Ereignisse, Startverriegelung und Prozessbereinigung. Weitere Tests prüfen blockierte Desktop-Aktionen, volle Warteschlangen, abgelaufene Aktionen, priorisiertes Beenden, harte Zeitgrenzen und Session-Bereinigung nach einem GPUI-Panic.

## Sichtprüfung

Das echte Fenster wurde mit temporärer Konfiguration gestartet. Geprüft wurden alle fünf Ansichten, die Seitenleiste, die kompakte Navigation bei 620 × 600 Pixeln, die dauerhafte Statuszeile, Kontaktanlage, Suche, Entfernen, DND und Passwortmaskierung. Audio-Geräte wurden über den vorhandenen PipeWire-Dienst aufgelistet.

## Lautstärkeregler

Der erste Prototyp regelte baresip-Streams. Das war nicht die gewünschte Systemlautstärke und wurde ersetzt. Der aktuelle Regler setzt per `wpctl` die Lautstärke des in Audio gewählten Ausgabegeräts. Ohne ausdrückliche Auswahl nimmt er das aktuelle Standardgerät.

- Plattformtests prüfen die Auflösung des Gerätenamens auf eine Sink-ID und das Parsen des `wpctl`-Wertes.
- Session-Tests prüfen das sofortige Setzen ohne Anruf, die Auswahl des Gesprächsgeräts statt eines abweichenden Klingelgeräts und den sekündlichen Abgleich bei geöffneter Audioansicht.
- GPUI-Tests prüfen Klicks auf den Regler, einen nicht verfügbaren Ausgang und das Verhalten bei älteren Snapshots.
- Ein temporärer Null-Sink zeigte `Volume: 1.00`, nach `wpctl set-volume` `Volume: 0.80`. Danach wurde er auf 1.00 gesetzt und entfernt. Andere Geräte blieben unverändert.

Ein vollständiger Anruf mit dem neuen Geräte-Regler wurde noch nicht geprüft. Bei einem früheren GUI-Test griff der Fokuswechsel über `hyprctl` nicht immer; einige Tastendrücke landeten in einem anderen Fenster. Deshalb wurden keine weiteren Tastatureingaben an das echte Fenster gesendet.

## Grenzen

Es gab keinen Anruf über das produktive SIP-Konto und keine Audioverbindung zu einer Gegenstelle. Reale Registrierung, Sprachqualität, TLS-/SRTP-Aushandlung und providerabhängige Fehler bleiben manuell zu prüfen. Die automatisierten Anruftests verwenden simulierte Ereignisse und Test-D-Bus-Dienste.

Rust-analyzer konnte ohne installierte Standardbibliotheksquellen keine verlässliche vollständige Diagnose liefern. Maßgeblich waren die erfolgreichen Compiler-, Test- und Clippy-Läufe. Cargo weist bei `proc-macro-error2` auf eine mögliche Inkompatibilität mit einer zukünftigen Rust-Version hin.

Kein Test stoppt, startet oder deaktiviert `baresip.service`.
