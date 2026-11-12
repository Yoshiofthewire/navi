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

## Frosted panels (2026-11-11)

Nested on X11 under KDE Plasma on Usagi, release builds, the dark `Next` 7680 × 2160 wallpaper,
every XDG directory in a temporary one, a private session bus. `scripts/measure-idle.py`, 60 s
after a 10 s warm-up, at the size KWin picked:

| Build | Navi + bar CPU (% of one core) | Navi + bar PSS at end |
| --- | ---: | ---: |
| `main` before frost | 0.05 | 145.3 MiB |
| With frost | 0.05 | 143.6 MiB |

Idle frames are unchanged: `src/render.rs` is untouched, and the only per-frame addition is
comparing the outputs' names and sizes with the last frost batch. At 3440 × 1440 with debug
logging (`navi::winit=debug`), the nested backend drew no frames in 30 s of idle beyond the
clock's minute tick, with frost as without. A frost batch (decode and blur, on its worker) took
89–144 ms in release, 450–840 ms in a debug build, once at startup, once per output-size
change and once per wallpaper change; never while idle.

navi-bar's draw time (`navi_bar=debug`, the `dock drawn` and `band drawn` lines, after the
first draw, which loads fonts and icons; `frost=true` only once the body really shows frost),
2040 × 1275 at scale 1, no resize:

| Surface | Solid body | Frosted body |
| --- | ---: | ---: |
| Dock | 0.03–0.10 ms | 0.13 ms |
| Status band (2016 px wide) | 0.22–0.52 ms | 0.8–1.6 ms |

The bilinear pattern fill costs about 1 ms on the widest panel, only when navi-bar redraws
(the clock's minute, hover, a menu), never per compositor frame.

## Settings memory (2026-11-10)

The shipped Settings default is Cairo. On Usagi at 1280 × 800, the release entrypoint
settled at 73.6 MiB PSS (96.8 MiB RSS) on Displays, with about 0.20% of one CPU core
over a 20-second idle sample. Explicitly selecting `GSK_RENDERER=gl` used 244.3 MiB PSS
in the same nested test. The earlier approximately 242 MiB OpenGL measurement does not
describe the default renderer. Renderer overrides remain supported.

Rebuilding Displays leaked its monitor diagram: the drag controller's callback captured
the widget that owned the controller. The callback now gets the widget from the gesture.
Leaving or rebuilding a page also clears the saved display fields, primary selector,
monitor hit targets and active drag, so hidden controls can be released.

A separate Cairo native-fixture stress run rebuilt Displays 1,000 times, sampling `/proc`
and Python widget counts. Both runs used the same private nested configuration and simulated
services, with no screenshots or input replay. They ran sequentially; swapped PSS was zero.

| After 1,000 rebuilds | Before | After |
| --- | ---: | ---: |
| Live monitor diagrams | 1,001 | 1 |
| Settings fixture PSS | 70.50 MiB | 66.16 MiB |
| Settings fixture RSS | 96.43 MiB | 91.17 MiB |
| PSS growth from rebuild 500 to 1,000 | 2.58 MiB | 0.12 MiB |

This prevents continuing retention; it does not establish a lower fresh-open footprint.
The fixture and packaged application have different startup overhead, and PSS varies with
shared libraries and residency. GPU memory is excluded. Real device operations and hardware
sessions were not benchmarked.

`src/settings/check_ui.py` includes a native lifetime regression: rebuild Displays 50 times,
simulate retained drag state, leave the page, and assert that every retired diagram's native
weak reference is dead. It fails on the original code. To increase its stress count, set
`NAVI_UI_CHECK_REFRESHES=1000` when running the disposable native UI check described in
[Settings validation](settings.md#validation). Its screenshots are for behavior verification,
not idle performance measurement.
