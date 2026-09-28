# Demo-Screenshots

12 native Aufnahmen der Anwendung mit erfundenen Daten, deutscher Oberfläche und dunklem Design.

- Desktop: Fenster mit 1100 × 760 Pixeln, Screenshot mit 1180 × 840 Pixeln, Navigation in der Seitenleiste.
- Kompakt: Fenster mit 600 × 800 Pixeln, Screenshot mit 680 × 880 Pixeln, Navigation oben.
- Rundum 40 Pixel Abstand mit dem echten Fensterrahmen und einem Ausschnitt des Wallpapers.
- Konto, Kontakte, Geräte, Registrierung und Anrufe sind simuliert. Das Demo-Backend startet kein baresip und liest keine persönlichen Kontodaten.

| Ansicht | Desktop | Kompakt |
| --- | --- | --- |
| Telefon | [![Telefon, Desktop](desktop-phone.png)](desktop-phone.png) | [![Telefon, kompakt](compact-phone.png)](compact-phone.png) |
| Kontakte | [![Kontakte, Desktop](desktop-contacts.png)](desktop-contacts.png) | [![Kontakte, kompakt](compact-contacts.png)](compact-contacts.png) |
| Konto | [![Konto, Desktop](desktop-account.png)](desktop-account.png) | [![Konto, kompakt](compact-account.png)](compact-account.png) |
| Anrufverlauf | [![Anrufverlauf, Desktop](desktop-history.png)](desktop-history.png) | [![Anrufverlauf, kompakt](compact-history.png)](compact-history.png) |
| Einstellungen | [![Einstellungen, Desktop](desktop-settings.png)](desktop-settings.png) | [![Einstellungen, kompakt](compact-settings.png)](compact-settings.png) |
| Eingehender Anruf | [![Eingehender Anruf, Desktop](desktop-incoming.png)](desktop-incoming.png) | [![Eingehender Anruf, kompakt](compact-incoming.png)](compact-incoming.png) |

## Aufnahmen erneut erstellen

Aus dem Projektverzeichnis in einer laufenden Hyprland-Sitzung mit `grim`:

```sh
cargo build --locked --example demo_screenshots
python3 scripts/capture-demo.py
```

Das Skript öffnet ein separates Demo-Fenster auf einem freien Workspace, setzt ausschließlich dieses Fenster auf deckende Darstellung und beendet es nach den Aufnahmen. Anschließend kehrt es zum vorherigen Workspace zurück. Die Demo verwendet ein temporäres Konfigurationsverzeichnis. Die Fenstersteuerung nutzt die Lua-Dispatcher von Hyprland.
