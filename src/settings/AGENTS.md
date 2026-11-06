# Native Settings application

## Purpose and ownership

`model.py` owns socket requests, settings search, desktop-service models and system information.
`services.py` owns NetworkManager/BlueZ D-Bus and native audio-client operations.
`system.py` owns the additional system-service controls and per-instance shell requests.
`extra_ui.py` extends the existing native forms; `app.py` owns GTK interaction and state.
`src/bin/navi-settings.rs` embeds the sources into one isolated Python invocation. The adjacent Rust `src/settings.rs` owns compositor-side settings.

## Local contracts

- Keep GTK imports in `run_settings` so model checks work without a display or GTK.
- Keep blocking I/O in the worker pool and mutate widgets on GTK's main loop.
- Apply device changes only on explicit user actions. Audio commands use bounded typed arguments
  and no shell; Wi-Fi passwords travel only through NetworkManager D-Bus, never logs or argv.
- Use Gio's native application associations and file picker; preserve keyboard and accessibility
  support supplied by the toolkit.
- NetworkManager owns Wi-Fi credential storage. Clear password widgets after submission/cancel.
  Only the current BlueZ bus owner may invoke pairing prompts for the explicitly requested device.
  Keep pairing confirmations explicit and cancel requests on app closure. Swaylock owns login authentication.

## Verification

`python3 -m unittest discover -s src/settings -p 'test_*.py'` exercises search, credential profiles, audio commands, pairing registration,
actual private-socket exchange and system summary. Verify rendered pages inside an isolated Navi
instance without changing host devices, application defaults, credentials or power state.
