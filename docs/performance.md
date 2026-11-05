# Performance

Initial targets for a release build: Navi + navi-bar below 0.5% of one CPU core while idle,
below 100 MiB PSS, and no continuous redraws of a static nested desktop. XWayland and apps
are reported separately. These are budgets, not guarantees across graphics drivers.

## First rendering pass (2026-11-05)

Measurements on Usagi, nested under KDE Plasma/Wayland, using AMD graphics and Mesa 26.2.5 at 1280 × 800, with the same imported
wallpaper, icons, dock pins and XWayland enabled. GPU VRAM is not included in `/proc` PSS.
No screen capture or input replay was enabled during CPU/memory measurements. The machine
was running other work, so shared-library PSS and paging varied; this is not an isolated lab.

| Scenario | Duration | Navi + bar CPU (% of one core) |
| --- | ---: | ---: |
| Before: static desktop, no apps | 60 s | 3.406 |
| After: static desktop, no apps | 60 s | 0.000 recorded |
| After: three static Wayland Konsole windows | 302 s | 0.006 |

The final empty-desktop run used 95.58–95.79 MiB PSS including swap (none swapped at the endpoint),
compared with 95.32 MiB resident PSS before the change. No RAM saving is claimed. XWayland
added 32.28 MiB at the end of the final empty run, making about 128 MiB for the two Navi
processes plus XWayland. The 50 MiB stretch goal has not been reached.

The three-window run started at 97.64 MiB PSS and ended at 81.61 MiB, with no swapped PSS at
its endpoint. Individual process resident peaks were 81.26 MiB (Navi) and 16.77 MiB (bar).
XWayland was 19.56 MiB at the endpoint, separate from the core; the three terminals and their
sleep processes were also separate. Falling PSS is not claimed as an allocation reduction:
sharing and residency can change while the desktop sits idle. The change primarily saves CPU.

CPU uses process user+system tick deltas divided by actual elapsed time. One-core CPU can
exceed 100% for multithreaded work. Small readings approach the sampler's tick resolution;
0.006% should be read as nearly idle, not as a precision guarantee. A recorded zero means no
additional CPU ticks were observed in that sample, not a guarantee of zero CPU use.

## What changed

The nested backend no longer requests its next frame unconditionally. Input, surface commits,
window/layer destruction, focus and layout changes signal a repaint through calloop. Repeated
signals coalesce and are paced to the nested output's advertised 60 Hz. Active drags retain
frames so the stationary half-second tab preview still changes. Failed frames are retried.

Decoration rectangles now retain their identities and commit counters through Smithay's
`SolidColorBuffer`. Identical decoration scenes produce no damage; moved, recolored, resized,
added and removed decorations still invalidate their regions. Both backends use this cache.

Window-list updates, popup cleanup and protocol flushing run independently of a rendered frame,
so an idle or obscured nested window does not stop serving clients. Screenshot instrumentation
adds an explicit one-second capture tick only when `NAVI_SCREENSHOT` is set.

The DRM backend still checks for damage on its existing 16 ms timer when idle. Stable decoration
identities let it recognize unchanged decorations, but its real CPU and GPU savings are unproven.
This pass does not establish full-session, multi-monitor, suspend/resume, or startup budgets.

## Repeat

Build as your normal user and run from an existing desktop:

```sh
cargo build --release --locked
python3 scripts/measure-idle.py --seconds 300 --output idle.json
python3 scripts/measure-idle.py --seconds 300 --windows 3 --output three-windows.json
```

The second command with `--windows 3` requires Konsole. It starts three separate native Wayland
terminals with their built-in profile, disables cursor blinking for a static workload, and runs
`sleep` without a shell. The script uses a temporary state directory, leaves your configuration
alone, removes capture/replay instrumentation, waits ten seconds, and samples `/proc` each second.
It stops only processes it launched. Keep the nested window visible and avoid interacting with it.
It never selects the hardware backend when no host display is present; it refuses to run instead.

JSON includes per-process CPU, initial/final/peak resident PSS, final swapped PSS, and initial/final
PSS including swap. Count resident plus swapped PSS when comparing allocation footprints under
memory pressure; report GPU buffers separately. App processes and XWayland are never silently
included in the Navi + bar figure.

Behavior checks also exercised real Wayland terminals, launcher/session menus, moving and
holding a window to form tabs, and X11 animation. An X11 gears window rendered 239 frames in
four seconds; closing it returned the desktop to zero frames over a subsequent four seconds.
A separate unchanged-desktop check saw zero new frames over five seconds. These frame-count
checks used debug logging, separately from the resource samples.
