# Messung des Ansichtswechsels

Der ignorierte Test `real_window_view_switch_latency` in `src/ui_render_bench.rs` öffnet ein echtes GPUI-Fenster mit simuliertem Backend und temporären Dateien. Er verwendet kein SIP-Konto und sendet keine Tastatur- oder Mausereignisse an den Desktop.

Nach 25 Aufwärmwechseln misst er 50 Wechsel pro Ansicht. Gemessen wird `change_view` bis zum Ende eines ausdrücklich angeforderten `Window::draw`. Das umfasst CPU-seitig Layout, Prepaint, Paint und den Abschluss der Scene. GPU-Ausgabe, Display-Latenz und die Zeit vom physischen Klick bis zur Eingabeverarbeitung sind nicht enthalten. Der erzwungene Draw umgeht die normale Planung und Invalidierung. Es handelt sich daher nicht um eine vollständige Klick-bis-Bild-Messung.

## Ergebnisse im Debug-Build

Der Fenstermanager wies dem Fenster 804 × 1394 Pixel zu. Der Umschalt-Handler selbst benötigte je nach Ansicht im Median 9 bis 22 Mikrosekunden.

| Ansicht | Umschalten und Scene-Aufbau, Median | p95 |
| --- | ---: | ---: |
| Phone | 61,86 ms | 69,12 ms |
| Contacts | 60,52 ms | 65,29 ms |
| Audio | 65,73 ms | 69,73 ms |
| Account | 76,15 ms | 80,40 ms |
| History | 35,65 ms | 38,94 ms |

Bei 60 Hz stehen rund 16,7 ms pro Bild zur Verfügung. Schon der CPU-seitige vollständige Scene-Aufbau überschreitet dieses Budget. Die Messung zeigt keinen langsamen Umschalt-Handler oder eine synchrone SIP-Abfrage.

Zum Messzeitpunkt installierte das Installationsskript `target/debug/gosiptea-client` ohne Optimierung. Build, Installation und Update wurden inzwischen auf `--release` umgestellt. Der vollständige Release-Build wurde installiert und mit dem isolierten Fenster-Starttest geprüft. Ein zweiter Versuch mit Optimierungsstufe 3 nur für GPUI und den Anwendungscode ist kein belastbarer Vergleich: Die übrigen Abhängigkeiten blieben unoptimiert, Debug-Assertions aktiv, und das Fenster erhielt eine andere Größe von 1154 × 697 Pixeln. Eine vollständige Release-Messung bei gleicher Fenstergröße steht aus. Zed wurde nicht vermessen.

## Wiederholen

```sh
CARGO_BUILD_JOBS=2 cargo test --locked --lib real_window_view_switch_latency -- --ignored --nocapture --test-threads=1
```

Der Test braucht eine laufende grafische Sitzung. Der Fenstermanager kann vorhandene Fenster beim Öffnen des Testfensters neu anordnen. Die tatsächlich zugewiesene Fenstergröße steht im Messprotokoll. Für Build-Vergleiche müssen Fenstergröße, Testdaten und Systemlast vergleichbar sein.

Die Einzelwerte dieser Messung liegen unter `/tmp/gosiptea-render-bench/debug-run-final.log`; die Datei ist nicht dauerhaft archiviert.
