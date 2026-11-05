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

## Evidence and Open Work

The user validated the merged lock-screen appearance and password unlock in nested Navi.
Physical multi-monitor, DRM and suspend validation remain separate hardware work.
Central account provisioning and SSO are future identity requirements, not implemented login features.
