# Navi

<!-- impeccable:product-schema 1 -->

## Platform

Native Linux desktop using Wayland. This platform is outside Impeccable's web/iOS/Android presets.

## Users and Purpose

Navi serves people using several large monitors who want predictable tiling with a mouse.
Windows retain their orientation, fit a fixed grid, and use tabs only when free space runs out.
A small separate Settings app makes desktop configuration accessible without editing files.

## Operating Context

Rust/Smithay compositor and separate shell, tested in a nested window before hardware use.
Settings opens on demand with native GTK 4 widgets. Built-in controls use NetworkManager and
BlueZ over D-Bus, and PipeWire-Pulse/PulseAudio through the native pactl client.

## Approved Settings Scope

Displays, wallpaper, network, Bluetooth, sound, power/locking, mouse/keyboard, default
applications, and About This PC. Search finds tasks across categories. Unavailable features
explain the missing service or supported tool; controls represent implemented behavior.
Display changes have an independent timed revert. Existing plain configuration remains usable.

## Product Principles

- The mouse is sufficient; keyboard shortcuts are optional.
- Keep the compositor small and keep Settings unloaded while closed.
- Preserve user configuration and running applications on failed changes.
- Use existing Linux services and native controls.

## Desktop Application Policy

- Non-terminal desktop utilities supplied or recommended as part of Navi must match Navi's
  selected appearance: color scheme, palette and accent, typography, icons, cursors and window
  styling. Theme compatibility must be verified before selecting a utility; a shared toolkit
  or dark-mode setting alone is insufficient. Terminal appearance is exempt from this rule.
- Desktop icons are prohibited, with no option to enable them. The user's Desktop directory
  remains accessible as an ordinary folder in the file manager.
- Navi as a desktop environment uses the browser the user brings and their chosen default
  associations. Browser installation is outside its app bundle.
- If Navi becomes a distribution, its bundled browser must be a degoogled Chromium variant
  selected for low resource usage and maintained security updates. Ungoogled Chromium is an
  initial candidate; being degoogled does not establish a smaller memory footprint, which
  requires measurement before selection.

## Evidence and Open Work

The user validated the merged lock-screen appearance and password unlock in nested Navi.
Physical multi-monitor, DRM and suspend validation remain separate hardware work.
Central account provisioning and SSO are future identity requirements, not implemented login features.
