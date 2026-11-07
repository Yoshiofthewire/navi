# Navi work guidance

## Purpose and ownership

Navi contains a Rust/Smithay compositor, its separate shell and locker, and an on-demand Settings
application. `docs/design.md` owns desktop behavior and visual tokens; `docs/settings.md` owns
Settings scope, configuration and the per-instance settings protocol.

## Local contracts

- Keep login authentication in swaylock/PAM. Settings has no unlock operation and accepts no
  login passwords. NetworkManager owns connection profiles; new remembered Wi-Fi passwords use
  oo7 through libsecret and a native NM agent.
- Keep Settings isolated from compositor GUI dependencies; native GTK runs only while open.
- Display changes retain an independent compositor timeout and restore on failure. Nested
  tests affect only their own Navi output; system-device controls explicitly name their scope.
- Settings requests are bounded data, use private runtime sockets, reject changes while locked,
  and preserve malformed configuration. Device controls use native services; audio arguments are
  bounded and never evaluated by a shell. Wi-Fi passwords use D-Bus, and pairing checks the BlueZ owner.
- Secrets: never replace an existing provider or edit PAM automatically. Read `docs/secrets.md`
  before changing vault startup, saved network secrets or the private-bus checks.
- Reuse `/home/yoshi/git/navi/target` for local Cargo across worktrees, with four jobs and
  6 GiB/no-swap systemd scopes on this host.

## Verification

Run CI's format, Clippy, debug/release tests/builds, XWayland and stock/patched locker integration,
and fail-safe startup checks before hand-back. Settings also has Python model/socket tests and
requires rendered native-app verification. Preserve the user's running preview sessions.

## Child DOX Index

- `src/settings/AGENTS.md` — native Settings UI/model and its headless checks.
