# Desktop utilities

Navi Files integrates an installed Thunar as the first utility using Navi's shared appearance.
Settings remains the existing native GTK 4 app. Utility support is verified per application;
further toolkit integrations must meet the [application policy](../PRODUCT.md#desktop-application-policy)
before their tools are recommended. Terminals keep their own appearance. The browser is supplied
by the user; a future distribution has a separate degoogled Chromium selection policy.

## Files

The launcher supplies **Navi Files** in System when `navi-files` is beside `navi-bar`. It is pinnable
and matches Thunar's running window in the dock. The regular Thunar launcher entry is replaced
inside Navi's menu. `navi-files [folder-or-URI ...]` also opens supplied locations; with no argument
it opens Home. Paths are separate arguments, including spaces and Unicode, with no shell evaluation.
The helper passes locations after `--`, so a filename cannot turn into a Thunar command option.

Thunar owns copying, moving, rename dialogs, symlinks, trash, Open With and MIME handlers. Existing
MIME defaults are preserved. Installing `dist/navi-files.desktop` offers Navi Files as a directory
handler in Default Applications; changing that default remains an explicit user action.

The Desktop directory is an ordinary file-manager location. Navi starts no desktop-icon service
and offers no desktop-icons toggle. Files is opened on demand, without Thunar's daemon flag.
An existing Thunar is reused only when its process uses this Navi instance's appearance file.
If Thunar is running elsewhere, a themed recovery window asks the user to close its windows first;
Navi leaves existing file operations running. This also prevents a nested preview from opening
an unthemed window on its host desktop. Missing Thunar or a missing theme module has a visible
recovery window rather than a silent launcher failure.
An existing daemon may need an explicit `thunar --quit` after its file operations finish.

Install Thunar and GVfs using the distribution's package manager. GVfs, UDisks and their normal
authorization handle trash browsing, removable media and network locations. Particular protocols
may need additional GVfs backend packages. Optional Tumbler supplies thumbnails. Navi does not
mount devices automatically or start an Xfce desktop/session. Its oo7/libsecret service remains
the full-session Secret Service; this integration adds no credential store.

## Shared appearance

`assets/utility.css` owns native utility control styling; colors resolve through `src/theme.rs`,
using the same palette as the compositor, shell and Settings. Navi's existing settings worker
writes a private, atomic `0700` directory/`0600` appearance file at
`$XDG_RUNTIME_DIR/navi-$WAYLAND_DISPLAY.utility/appearance.ini`. Validated settings determine
colors, accent, high contrast, text size, icon theme and cursor theme/size. The default icon
choice follows Navi's KDE import/default resolution.

The 17 KiB native GTK 3 module `libnavi-gtk3.so` loads only in the selected utility process through
GTK's supported module hook. It applies a CSS provider and GTK settings, with a directory monitor
for live saves. The single bounded appearance file avoids partial CSS/settings transactions.
Native keyboard handling, focus, menus, dialogs and accessibility remain with GTK/Thunar. Minimize,
maximize and close controls stay visible. The bridge never rewrites shared GTK/Xfce appearance
preferences; Thunar keeps its normal file-manager preferences. The module is neither loaded nor
linked into the compositor.

Keep the module beside `navi-files`, including when installing release artifacts. Building it
needs GTK 3 headers (`libgtk-3-dev` on Debian/Ubuntu, `gtk3` on Arch) and the existing C compiler;
the compositor itself retains its existing runtime dependencies. Recovery windows use Settings'
Python/PyGObject/GTK 4 runtime and default to Cairo unless a renderer preference was supplied.

## Verification

After a debug build:

```sh
python3 scripts/check-utilities.py
```

It writes into a new private temporary directory. `--output <dir>` must be empty, yours and mode
700; a symlink is refused. The check needs Thunar, Xvfb, GTK 3 GI and the normal Navi
dependencies. CI installs them and runs it explicitly, uploading native captures and results.
A private session bus, Xvfb display, temporary Home/XDG roots and an unavailable
system bus isolate the check from the host. The actual native module must apply live dark, light
and high-contrast palettes, font sizes, icons, cursors and window-control layout. Actual Thunar
captures wait for rendered content colors, rather than assuming a timing delay means a repaint.

Real Thunar D-Bus operations copy, move, symlink and trash temporary files; a temporary MIME
handler records a real Open action. Unicode filenames, the Desktop folder, second launches,
foreign-instance refusal and missing-Thunar recovery are checked. Captures contain only Navi's
own output. `ldd` verifies that the compositor does not depend on GTK.

Removable-drive mounting/ejection, authenticated network shares and their authorization prompts
remain supervised device/service checks. The private-bus test exercises local files and trash;
it does not claim physical-device or remote-provider coverage.

### Resource sample (2026-11-10)

On Usagi, distribution Thunar 4.20.10 with the utility module used 30.15–30.66 MiB PSS during
a 20.03-second static-folder sample after five seconds of warmup; final RSS was 52.30 MiB,
with no swapped PSS. No additional CPU ticks were observed, which is not a guarantee of zero
CPU usage. The module itself was 17 KiB in this build.

This measured only the file-manager process, inside a private nested Navi at 1280 × 800,
with a two-entry test folder and no screenshots/input replay. The launcher had already exec'd
Thunar, so no Python or wrapper process was resident. Navi, XWayland, Xfconf, portals and
optional GVfs/thumbnail services are separate costs. PSS varies with library sharing and
residency; GPU memory, large directories, thumbnails, network operations and hardware sessions
were not benchmarked.
