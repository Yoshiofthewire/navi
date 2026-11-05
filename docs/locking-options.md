# Screen-locking options for Navi

Research date: **2026-11-05**. Decision support only: no packages installed, locks triggered, power operations run, or host configuration changed. Sources below are upstream READMEs, manifests, implementations, specifications and release APIs read on this date. Compatibility is a source review, **not a successful Navi runtime test**. No CPU/RAM measurements or independent security-audit claims are made.

## Recommendation

**Implement correct compositor session locking, then validate upstream swaylock first.** It already provides a plain color or image, password feedback and PAM without adding GTK, EGL/GPU rendering or Hyprland libraries. Upstream explicitly targets any compositor implementing `ext-session-lock-v1`; the source uses ordinary shared-memory buffers. It is the simplest practical default for Navi's lightweight scope, subject to the core-protocol versions below. [S1][S2][S3]

**Waylock is the strongest alternative if three-color feedback is enough.** Its dependency surface is smaller, but the inspected implementation also *requires* viewporter and single-pixel-buffer; ext-session-lock alone is insufficient. Choose it if Navi already exposes those protocols or their addition is independently worthwhile. This is an architectural comparison, not a measured memory ranking. [W1][W2][W3]

Keep **gtklock** for a deliberately GTK/CSS-oriented screen and **hyprlock** for a deliberately rich GPU-rendered screen. Both are real session-lock clients; neither requires replacing Navi with a wlroots compositor. Their additional toolkit/rendering requirements still need testing. gtklock currently asks for a new maintainer; hyprlock brings more libraries and graphics assumptions. [G1][G2][G3][H1][H2][H3]

## What is actually being secured

`ext-session-lock-v1` divides responsibilities: **Navi owns the lock and isolates normal clients; the locker owns authentication and requests unlock.** The protocol requires normal clients to stop receiving input/rendering, all outputs to hide sensitive content before `locked`, and a locker crash to leave the session locked. A fullscreen window, layer-shell overlay, password dialog or PAM agent alone does not establish those guarantees. The protocol permits compositor policy to restrict which clients can acquire the lock. [P1]

PAM is the separate local authentication mechanism. A polkit authentication agent authorizes privileged operations through polkit; it does not become the session's unlock dialog simply because both ask for a password. [A1][A2]

## Four practical standalone choices

| Locker | Appearance and authentication | Active dependencies / license | Fit for Navi |
| --- | --- | --- | --- |
| **swaylock** | Solid color or image, configurable ring/text feedback; PAM by default, optional shadow-password build. No built-in blurred desktop screenshot. [S1][S2][S4] | C; Wayland, xkbcommon, Cairo; optional gdk-pixbuf for non-PNG images; PAM recommended. MIT. [S1][S2] | **First choice.** Shared-memory rendering; no layer-shell, private protocol, EGL or DMA-BUF requirement in inspected client. [S2][S3] |
| **waylock** | Initial/input/failure colors; type password and Enter. No image/clock/widgets. PAM. [W1] | Zig build; Wayland, xkbcommon, PAM; no Cairo/GTK/GPU renderer in manifest. ISC. [W1][W2] | **Best minimal alternative.** Requires viewporter + single-pixel-buffer in addition to session-lock; do not assume it works with the same globals as swaylock. [W3][W4] |
| **gtklock** | GTK widgets, CSS theming and optional modules for richer lock UI; PAM. [G1][G2][G4] | C; GTK **3**, gtk-session-lock, GModule, PAM. GPLv3; gtk-session-lock has GPLv3 terms with some MIT files. Build manifest is authoritative where distro snippets disagree about GTK version. [G2][G3] | **Conditional option.** Modern gtklock uses gtk-session-lock, not an insecure layer-shell-only lock. Adds the GTK/GDK stack and module maintenance; upstream is seeking a maintainer. [G1][G2][G3] |
| **hyprlock** | Images/screenshots, blur, clocks/labels, gradient borders, shadows, animations; PAM and native fprintd support. [H1][H5] | C++23; EGL + GLES3 + Wayland-EGL, Cairo/Pango, xkbcommon, GBM/DRM, sdbus-c++, hyprlang/hyprutils/hyprgraphics; scanner at build time. BSD-3-Clause. [H2][H6] | **Richest shortlisted option.** Source uses standard/core protocols rather than a mandatory Hyprland-private lock API. Requires a working client EGL path; screenshot capture is an additional feature, not the locking mechanism. [H2][H3][H4] |

### Protocol requirements beyond the lock extension

These are the **inspected current upstream source's bindings/checks**, not promises about every historical distro build. Every row also requires usable outputs, a keyboard seat and compatible buffer handling. Test the selected packaged version on Navi.

| Client | Required/used core globals and extra requirements | Optional features / caveats |
| --- | --- | --- |
| swaylock | Binds `wl_compositor` **4**, `wl_shm` **1**, `wl_seat` **4**, `wl_output` **4**, and `ext_session_lock_manager_v1` **1**; also binds `wl_subcompositor` **1** if advertised. Its manifest generates only the session-lock extension. [S2][S3] | No viewporter/fractional-scale/layer-shell requirement in inspected source. Static background images need no screenshot protocol. [S2][S3] |
| waylock | Explicitly requires `wl_compositor >= 4`, `wl_output >= 3`, `wl_seat >= 5`, session-lock **1**, **`wp_viewporter` 1** and **`wp_single_pixel_buffer_manager_v1` 1**. Missing either extra manager is fatal. Uses single-pixel buffers stretched by viewporter, not a `wl_shm` fallback. [W2][W3][W4] | No fractional-scale, layer-shell, EGL or screenshot requirement in inspected code. [W2][W3][W4] |
| gtklock / gtk-session-lock | gtk-session-lock itself binds session-lock **1** on the existing GTK3/GDK Wayland connection. GTK/GDK supplies the normal Wayland client infrastructure; this review did **not** establish a complete version-by-version minimum for that backend. [G2][G3][G5] | It derives code from gtk-layer-shell but is a different library and lock protocol. Do not impose layer-shell merely because of that ancestry. One lock surface per output means toolkit popup windows/tooltips are limited. Test installed GTK/backend behavior. [G3][G5] |
| hyprlock | Inspected source binds `wl_compositor` **4**, `wl_output` **4**, `wl_seat` **8**, `wl_shm` **1** and session-lock **1**. EGL/GLES3 and Wayland-EGL rendering must work. It binds these core versions directly, so exposing lower advertised versions is a compatibility concern. [H2][H3][H4] | Fractional-scale **1** + viewporter **1** are optional together; integer-scale fallback exists. Cursor-shape is optional. Screenshot backgrounds use wlr-screencopy **3**; its DMA-BUF capture path only binds linux-dmabuf **4**, and can be skipped/changed by screenshot configuration. Do not equate this capture requirement with all EGL buffer-import requirements of the graphics driver. [H3][H4] |

### Upstream health and version evidence

A recent push is an activity signal, not proof of correctness or support. Dates here are UTC except the Codeberg tag's explicitly stated offset. GitHub `updated_at` can reflect non-code activity, so this table uses `pushed_at` plus releases/tags instead. [M1][M2][M3][M4]

| Project | Observed upstream state on research date |
| --- | --- |
| swaylock | Latest GitHub release **v1.8.6**, published **2026-07-09 15:59:34Z**; repository pushed **2026-10-29 08:38:10Z**, not archived. [S5][M1] |
| waylock | Canonical repository is **Codeberg**, with GitHub a read-only mirror. Codeberg's newest returned tag **v1.6.0** has tag commit timestamp **2026-04-27 11:39:54+02:00**; tag message says Zig 0.16 update with no intended behavior changes. GitHub mirror pushed **2026-05-21 07:07:01Z**, not archived. Its GitHub Releases page is stale relative to tags; do not call v0.6.4 the current version. The fetched master build file still declares `1.4.0-dev`, another reason to distinguish tag metadata from branch text. [W1][W2][W5][M2] |
| gtklock | Latest release **v5.0.0**, published **2026-10-27 21:43:26Z**; source manifest agrees. README prominently says **looking for a new maintainer**. Its required gtk-session-lock repository last pushed **2024-04-21 15:28:42Z**. [G1][G2][G6][M5] |
| hyprlock | Latest release **v0.9.7**, published **2026-11-01 18:12:42Z**; repository pushed **2026-11-03 15:10:43Z**, not archived. [H7][M4] |

## Authentication is a choice too

All four shortlisted clients can use a distro-managed PAM service. PAM configuration, available hardware, enrolled credentials and the application's conversation handler determine what actually works; adding `pam_fprintd`, FIDO/U2F or OTP modules is not a guarantee of a usable parallel or multi-step UI. [A1][A3][S6][W6][H5]

- **swaylock:** its PAM conversation supplies the submitted password to one echo-on/off prompt and aborts if asked again in the same conversation. It does not expose arbitrary successive prompt text as a rich login form. Verify any OTP/smart-card workflow against that implementation. Prefer its ordinary PAM package over configuring the non-PAM shadow/setuid fallback unless that is an explicit system requirement. [S1][S6]
- **waylock:** its PAM conversation fills echo-off responses from its current password buffer and ignores/logs echo-on, informational and error messages; its public UI remains three colors. That is a weak fit for workflows requiring visible challenge text or different responses in one transaction. [W1][W6]
- **gtklock:** GTK makes richer visible interaction possible, but optional modules and the exact PAM conversation still need validation with the intended distro stack. Its documented dependencies do not imply built-in native fingerprint UI. [G1][G2][G4]
- **hyprlock:** has a changing PAM prompt handler and a separate native fingerprint path via fprintd/D-Bus. That can offer a better fingerprint experience than running everything through one blocking PAM conversation, but needs the daemon, enrolled supported hardware and policy. Upstream source recommends its native fingerprint option over `pam_fprintd`; that recommendation is not a hardware compatibility guarantee. [H1][H5][H8]

This report does not recommend enabling biometrics, changing PAM policy, or adding passwordless grace periods as part of the first lock implementation. Choose and test the baseline password flow first; separate policies can be evaluated afterwards. That is a scope recommendation, not a claim that PAM only supports passwords. [A1][A3]

## Other real alternatives, grouped by practical fit

### swaylock-effects and forks

The familiar **mortie/swaylock-effects** adds screenshot capture, blur/pixelation, clocks and grace-period behavior. Its successor **jirutka/swaylock-effects** README says mortie's fork is no longer maintained; jirutka's own repository description now says **unmaintained**, with last push **2024-03-08**. Mortie's last push is **2023-11-28**. These are not a good default for a new security boundary. [E1][E2][E3]

There are further forks: **superty/swaylock-effects**, last pushed **2026-02-17**, advertises percentage positioning and PAM session support; **marzzzello/swaylock-effects**, last pushed **2024-09-11**, advertises fingerprint/scripts/input-while-validating. A newer fork's timestamp does not establish that upstream fixes were merged or its authentication changes are sound. They are candidates only if a specific effect is required and the chosen fork is reviewed. The inspected effects manifest includes layer-shell and wlr-screencopy alongside ext-session-lock: screenshot/effects feature compatibility needs checking separately. Pre-blurring an image used by upstream swaylock avoids taking on an effects fork solely for a blurred static background. [E1][E4][E5][S4]

### Rust and tiny alternatives

- **swayward-wm/swaywardlock** is a real standalone Rust client using Smithay's renderer, PAM, session-lock and EGL/OpenGL ES. It offers hyprlock-style widgets, screenshots and fingerprint support; README explicitly calls it **beta** and GPL-3.0-or-later. Last observed push **2026-10-28**. It is relevant if Rust/custom visuals matter, but its own testing/review claims are not an independent security audit, and using Smithay does not make Navi support automatic. [R1][R2]
- **dani-77/amorphis** is a real Rust/PAM session-lock client, with solid/image backgrounds and optional capture/effects. Upstream explicitly calls it **early-stage** and warns against using it as sole protection for an important session because the full hostile-input/suspend/multimonitor matrix is incomplete. MIT; last observed push **2026-10-24**. A credible experiment, not a production-default recommendation. [R3][R4]
- The name **wlock** is ambiguous. **fransac/wlock** is current C, GPLv3, ext-session-lock and flat colors, but authenticates with shadow/`crypt` and privilege setup rather than PAM. **edzdez/wlock** is OCaml with Cairo/PAM, including an optional separate fingerprint service. Neither is Rust. **rottencandy/wlock** is the real Rust example found: upstream description says **WIP / experimental**, manifest v0.0.1, Wayland-EGL/OpenGL/PAM dependencies, last push **2023-06-19**; no license was identified in the inspected manifest/repository metadata. Do not confuse these projects. [R5][R6][R7][R8]
- **wl-screenlock:** no maintained primary upstream was verified under that exact name in this research. Treat it as unverified rather than inventing dependencies, license or compatibility. This is a search limitation, not proof that no such project exists.
- A **custom Navi-branded Rust locker** is feasible as a separate session-lock client with a PAM helper, but means owning credential handling, PAM conversation, rendering, output changes, process lifetime and packaging. Reusing a locker first keeps that work out of the initial compositor security change. The protocol intentionally separates the client's authentication role from the compositor. [P1]

### Desktop-integrated lockers

**Quickshell-based shell lock screens** are another credible route to a custom UI. Quickshell provides `WlSessionLock` / `WlSessionLockSurface`, backed by ext-session-lock, and a separate `secure` property that becomes true after the compositor confirms coverage. It is a QtQuick toolkit primitive, not a complete authentication policy: the example explicitly unlocks on a button, so a real shell must connect successful authentication to unlock. That shifts application correctness and extra shell dependencies into the selected shell/configuration. Worth considering if Navi already adopts such a shell; otherwise an established standalone locker is simpler. Quickshell includes LGPLv3 licensing; review the chosen shell's separate license. [Q1][Q2]

| Option | Why it is not a simple standalone replacement |
| --- | --- |
| **KDE KScreenLocker** | Actively maintained Plasma architecture with QtQuick, KDE Frameworks, PlasmaQuick, LayerShellQt and PAM. The greeter uses layer-shell on Wayland; KSldApp can pass an inherited `WAYLAND_SOCKET` from the compositor. This is a coordinated Plasma/KWin lock architecture, not an ext-session-lock client that becomes secure by launching `kscreenlocker_greet` under Navi. Porting its UI would be a substantial integration project. Licenses vary by file (GPL/LGPL/BSD); do not label the whole tree with one guessed license. [D1][D2][D3][D4] |
| **GNOME lock screen** | Implemented inside GNOME Shell's ScreenShield/UnlockDialog environment and built against a specific libmutter API. It is not a separate generic locker binary for Navi. Current manifest is GNOME Shell **51.1**, GPL-2.0-or-later. Running a D-Bus lock command is an instruction to that desktop's existing lock owner, not a portable replacement for it. [D5][D6] |
| **COSMIC locker** | A more relevant Rust alternative than GNOME/KDE: `cosmic-greeter` dispatches to its locker for an ordinary user, and the locker really uses libcosmic/Iced session-lock operations plus PAM (`cosmic-greeter` service). However, it also pulls libcosmic, COSMIC settings/config/greeter-daemon/protocol dependencies and logind integration. GPL-3.0; current source demonstrates a real locker, but this review has not proven it runs usefully on Navi without COSMIC services. Consider a separate compatibility experiment if the COSMIC appearance/integration is desired. [D7][D8][D9][D10] |

### Tools that are not Wayland session lockers

**slock, i3lock and xsecurelock** use X11 locking mechanisms; an XWayland application cannot thereby lock Navi's native Wayland clients. They are not drop-in options for this compositor. xsecurelock explicitly documents that its Wayland-session X11 subset is not useful to lock. An existing X11 system can still legitimately use these tools. **Current XScreenSaver Wayland support was not verified:** its upstream site/FAQ returned HTTP 403 during research, so this report makes no categorical claim about current releases. Any X11-only locking path still cannot secure native Wayland clients. [X1][X2][X3][X4][P1]

**Wayfire/sway-specific plugins, launchers and idle helpers** must be evaluated by role. A Wayfire compositor plugin needs porting; `swayidle` is portable where its required idle protocol is available but remains an event daemon, not a password UI. Old layer-shell-only hacks must not be presented as equivalent to session-lock. [I1][I2][P1]

**loginctl, systemd, swayidle and wayidle are orchestration.** `loginctl lock-session` asks the session to lock *if it supports locking*. swayidle executes configured commands on idle, logind lock and before-sleep events; current upstream asks for `ext-idle-notify`. wayidle schedules commands after idle; its implementation binds `ext_idle_notifier_v1` even though its brief README confusingly links session-lock. None supplies the compositor enforcement and authentication UI by itself. swayidle explicitly executes its configured commands through a shell; it is an available integration option, not a recommendation to add a daemon or shell-based command path to Navi. For suspend, readiness must mean the session-lock `locked` event after all outputs are covered, not merely “the process started.” swayidle documents `-w` plus swaylock `-f` to wait for locking before releasing the sleep inhibitor. [I1][I2][I3][I4][P1]

## Suggested evaluation sequence

1. Finish and test Navi's compositor-side security policy before treating any locker as usable. The exact Smithay integration and threat checks are a separate section below.
2. In an isolated/test session, validate the selected distro **swaylock** with password PAM, multiple outputs, monitor removal/addition and a killed locker. Record versions and protocols. Do not run destructive tests against the user's current desktop.
3. Compare **waylock** only after checking viewporter + single-pixel-buffer exposure. If the flat screen is acceptable and no new compositor work is needed, it is a reasonable alternate default.
4. Trial **gtklock** or **hyprlock** only if their richer UI is wanted. For hyprlock, first use a solid/image background and no capture dependency; then test screenshot and fractional scaling separately. For gtklock, start without optional modules.
5. Benchmark only after runnable candidates exist. Measure idle and typing/animation behavior with the same outputs/theme; no source-only RAM/CPU ranking is justified.

## What every choice requires in Navi

The current code uses Smithay **0.7.0**. That version already supplies `SessionLockManagerState`,
`SessionLockHandler` and lock-surface plumbing. Its example explicitly leaves clearing the screen,
presenting a protected frame on every output, routing input and unlock policy to the compositor.
Implementing the handler is necessary; delegating the protocol is not the whole feature. [N1][N2][P1]

Navi currently has no session-lock, viewporter, single-pixel-buffer, idle-notify or capture manager
wired into its frontend. Smithay has modules for the first four, so waylock's extra protocols are
feasible additions, not a reason to replace Smithay. The existing compositor uses Smithay's default
`wl_compositor` 5, `wl_subcompositor` 1 and `wl_output` 4 globals; these fit swaylock's inspected
core bindings. Package-version and authentication behavior still need runtime testing. [N2][N3][S3][W3]

The common implementation should:

1. **Cover all outputs before reporting ready.** Blank and deny normal app input immediately;
   render only authorized lock surfaces (or the compositor's opaque fallback). Send the protocol's
   `locked` acknowledgement only after protected frames have actually been presented on every
   active output. A new/hotplugged output must start protected too. This is part of the protocol,
   not a special capability unique to a particular locker. [P1][N1]
2. **Keep the lock after a crash.** Keep compositor lock state independent of the locker process.
   A closed connection, missing surface or process exit must never reveal the desktop. Recovery
   can relaunch a trusted locker while the opaque fallback remains. Unlock on the authorized
   protocol request, not merely on a process exit code. Decide who can obtain the lock: the
   protocol permits a compositor-launched private client, which fits Navi's existing private-bar
   connection pattern. The locker need not inherit all of the bar's privileges. [P1][N2]
3. **Audit the whole input/focus path.** Lock surfaces must own keyboard/pointer input; regular
   apps, layers, notifications, XWayland focus and activation requests must not regain it.
   Clipboard/capture behavior must be defined for locked mode. Navi's existing unconditional
   Ctrl+Alt+Backspace quit and window-management shortcuts need an explicit locked-state policy;
   an emergency action must not expose or resume the unlocked desktop. Test VT return, scaling,
   dock removal/reconnection and a dead locker, including the nested backend. [P1][N2]
4. **Sequence sleep and user switching.** The menu currently calls login1 directly and warns that
   these operations do not lock. Change it to wait for confirmed lock coverage first. Also handle
   logind's external `Lock` / `PrepareForSleep` signals for lid closure and other suspend paths,
   using a delay inhibitor while locking. `loginctl lock-session` only sends a request; successful
   process startup or a successful D-Bus call is not proof that all displays are protected.
   Delay inhibitors have a maximum wait, so a timeout/failure policy must be tested. [N2][L1][L2][P1]
5. **Keep passwords out of the compositor.** Run the selected foreground locker on demand and
   let its PAM implementation and distro service authenticate. Navi controls isolation and
   readiness; a polkit agent is a separate feature. For swaylock, upstream provides `--ready-fd`
   after the compositor confirms coverage, although Navi can coordinate from its own lock state.
   Avoid introducing `sh -c` command chains into Navi's current direct-argv launch policy. [S4][S6][A1][A2][N2]

### Performance implications

No shortlisted locker was benchmarked on Navi: the prerequisite lock protocol is absent.
Dependencies and rendering architecture are evidence for what to test, not numerical RAM rankings.
Launch a standalone locker only when needed so it does not become an additional always-running
idle process. A static color/image should settle after presentation; blur, live screenshots,
widgets and animations need separate locked-state CPU/GPU measurements. Measure Navi + bar,
locker/PAM helper, and total session memory separately, using resident plus swapped PSS and
reporting GPU buffers separately, as in the existing performance methodology. [S2][W2][H2][N4]

For the first comparison, use the same outputs, plain/static background, password PAM policy and
idle duration for swaylock and waylock. Then test wrong-password handling, crash/recovery, sleep
and hotplug. Only add fingerprint, OTP, screenshot capture or animated themes when their distinct
requirements and behavior have been verified. This is an evaluation sequence, not a claim that
any candidate already passes Navi's lock tests.

## Central provisioning and SSO

**Choosing a session-lock client does not rule out either central provisioning or organization SSO.** It can limit the **unlock conversation** Navi can present. Keep the compositor's standard lock protocol independent of the Linux identity backend and greeter, and validate the intended enterprise PAM flow before making a locker permanent. A successful browser login at the greeter does not establish that the same flow works at the lock screen. [P1][SS1][SS2][SS3][SS4]

### Two identity functions, plus Linux integration

- **SCIM provisions identities.** RFC 7643 defines users/groups and attributes such as `id`, `externalId` and `active`; RFC 7644 defines HTTP operations for managing those resources. This is not an interactive login protocol. The service provider defines the precise meaning/enforcement of `active`; receiving a disabled-user update does not itself terminate a Linux session. [SS1][SS2]
- **OIDC/SAML authenticate or convey authentication.** OIDC adds end-user authentication to OAuth 2.0; SAML defines authentication assertions and protocols. The organization needs an appropriate relying party/broker and supported flow, not a SCIM password field in the locker. OAuth device authorization provides a URL/code for authentication in a browser, often on another device. [SS3][SS4][SS5]
- **Linux still needs a usable account.** An identity bridge must expose the user's name, stable numeric UID/GID, groups, home and shell through NSS or local account records, and integrate authentication/access policy through PAM. Home creation and session setup are login responsibilities; `pam_mkhomedir`, for example, creates a missing home when a session begins and does not delete it at logout. SCIM identifiers must be deliberately mapped to this account lifecycle. Offboarding must define account blocking, existing sessions, offline access and retained files; UID reuse can grant a new identity access to an old identity's files. These concerns belong to the identity/session integration, not the choice of lock-screen renderer. [SS1][SS6][SS7][SS8][SS9]

### Practical bridges to evaluate

**Canonical authd is a concrete browser-SSO route for supported Ubuntu deployments.** Its architecture supplies PAM and NSS modules plus provider brokers. Current stable documentation lists **Microsoft Entra ID, Google IAM and Keycloak**, with provider-specific device-code flows; it does not promise arbitrary IdP compatibility. Its documented GDM flow displays a QR/login code, completes authentication on a second device, then establishes a local password. That separates initial SSO onboarding from later local/offline authentication and makes a simple local-password unlock path worth testing. It is not a turnkey compatibility promise for Arch, Navi, every display manager or any shortlisted locker. Accounts and home directories are created after the first successful broker login: this is useful just-in-time onboarding, **not evidence of a general SCIM provisioning endpoint**. A separate supported provisioning integration is still needed if SCIM must pre-create/disable accounts. [SS8][SS10][SS11][SS12]

Authd's documented GDM integration should not be assumed to work through an arbitrary greeter or locker simply because PAM is installed. Its broker/capability negotiation and available UI matter. Its security documentation also makes offline policy explicit: local authentication is allowed by default when the provider is unreachable, unless the provider-access check is required. Test the organization's revocation/offline requirements against the chosen versions and flow. [SS9][SS10][SS11]

**SSSD remains an important NSS/PAM bridge for LDAP, AD, Kerberos and FreeIPA deployments, but “SSSD can never do IdP login” is now too broad.** Current upstream source also documents an **IdP provider** for OAuth 2.0/device authorization plus provider-specific REST identity lookup. It explicitly names **Entra ID and Keycloak**, requires device-authorization endpoints and suitable user/group API permissions, and generates POSIX attributes with documented UID/GID collision considerations. This is not a generic SCIM endpoint or assurance that every OIDC/SAML IdP works. Check the actual distro release, enabled providers, tenant permissions and supported PAM UI before choosing between it, authd or another maintained bridge. [SS13][SS14]

### First login and session unlock need different tests

The **greeter** signs in a potentially new user and starts a session. The **locker** authenticates the owner of an existing session and asks Navi to release its lock. An organization may accept browser SSO/MFA at first login plus a fresh local password/PIN or hardware interaction at unlock. If it instead requires fresh IdP MFA at every unlock, that is an additional compatibility requirement. Ordinary browsers inside the locked desktop cannot simply become the authentication UI: normal clients must remain isolated; use a supported locked-screen flow or an external device. [P1][SS5][SS7][SS11]

| Locker | Inspected PAM conversation and enterprise implication |
| --- | --- |
| **swaylock** | Supplies the submitted value to one echo-on/off prompt, then aborts if asked for another in that conversation; ignores PAM info/error messages. Good candidate for a verified single-response credential path. It cannot be assumed to present a device URL/code, broker selection, or password-then-OTP sequence. [S6] |
| **waylock** | Supplies the current password buffer to echo-off prompts; echo-on/info/error messages are ignored and logged. Its three-color UI does not present general challenge text or collect distinct successive answers. [W1][W6] |
| **gtklock** | Despite the richer GTK UI, current `auth.c` copies the **same submitted password** into echo-off and echo-on responses; PAM info/error text is forwarded for display. It does not thereby become an arbitrary multi-step PAM conversation. A theme/module should not be assumed to change that authentication behavior. [SS15] |
| **hyprlock** | Waits for input on initial/changed echo-on/off prompts, so it is a stronger candidate for successive distinct prompts. Repeated identical prompts may reuse an answer; general PAM info/error text is logged, with only selected messages copied into UI state. Device-code instructions, QR display, broker selection and cancellation still need testing; richer visuals alone do not establish compatibility. [H5] |

**Revocation requires a separate check:** none of these four inspected authentication paths calls `pam_acct_mgmt()`. Linux-PAM defines that as the account-validity/access check, distinct from `pam_authenticate()`. Therefore an access restriction configured **only** in a PAM `account` stack must not be assumed to run at unlock; verify backend/auth-stack enforcement and policy for a user disabled centrally during an existing session. [S6][W6][SS15][H5][SS16]

**Recommendation for Navi:** use swaylock for protocol bring-up and a tested simple credential flow, while evaluating the target IdP, Linux bridge and greeter independently. Before committing to a permanent locker, test first-time SSO onboarding, subsequent login, actual unlock prompts, wrong/cancelled/expired challenges, offline behavior and disabled-account behavior with the intended versions. Change the locker only if its verified authentication conversation requires it. Never treat possession or silent refresh of an existing SSO token as proof that the person at the locked device is present: refresh tokens obtain access tokens; an online reauthentication design must explicitly enforce the required freshness/user-verification policy. OIDC supplies mechanisms such as `max_age` and `auth_time`, but their availability does not prove that a particular Linux broker/locker implements them. [SS3][SS17]

## Primary sources

- [P1] [Wayland ext-session-lock-v1 specification](https://gitlab.freedesktop.org/wayland/wayland-protocols/-/blob/main/staging/ext-session-lock/ext-session-lock-v1.xml).
- [S1] [swaylock README](https://github.com/swaywm/swaylock/blob/master/README.md); [S2] [manifest](https://github.com/swaywm/swaylock/blob/master/meson.build); [S3] [protocol bindings / lock lifecycle](https://github.com/swaywm/swaylock/blob/master/main.c); [S4] [manual](https://github.com/swaywm/swaylock/blob/master/swaylock.1.scd); [S5] [latest-release API](https://api.github.com/repos/swaywm/swaylock/releases/latest); [S6] [PAM conversation](https://github.com/swaywm/swaylock/blob/master/pam.c).
- [W1] [waylock canonical README](https://codeberg.org/ifreund/waylock/src/branch/master/README.md); [W2] [build manifest](https://codeberg.org/ifreund/waylock/src/branch/master/build.zig); [W3] [required globals](https://codeberg.org/ifreund/waylock/src/branch/master/src/Lock.zig); [W4] [single-pixel viewport surfaces](https://codeberg.org/ifreund/waylock/src/branch/master/src/Output.zig); [W5] [canonical tag API](https://codeberg.org/api/v1/repos/ifreund/waylock/tags?limit=3); [W6] [PAM conversation](https://codeberg.org/ifreund/waylock/src/branch/master/src/auth.zig).
- [G1] [gtklock README / maintainer appeal](https://github.com/jovanlanik/gtklock/blob/master/README.md); [G2] [build manifest](https://github.com/jovanlanik/gtklock/blob/master/meson.build); [G3] [gtk-session-lock README / license / limitations](https://github.com/Cu3PO42/gtk-session-lock/blob/master/README.md); [G4] [gtklock wiki](https://github.com/jovanlanik/gtklock/wiki); [G5] [gtk-session-lock Wayland binding](https://github.com/Cu3PO42/gtk-session-lock/blob/master/src/gtk-wayland.c); [G6] [latest-release API](https://api.github.com/repos/jovanlanik/gtklock/releases/latest).
- [H1] [hyprlock README](https://github.com/hyprwm/hyprlock/blob/main/README.md); [H2] [build dependencies / protocols](https://github.com/hyprwm/hyprlock/blob/main/CMakeLists.txt); [H3] [registry / capture selection](https://github.com/hyprwm/hyprlock/blob/main/src/core/hyprlock.cpp); [H4] [lock-surface EGL / optional fractional scaling](https://github.com/hyprwm/hyprlock/blob/main/src/core/LockSurface.cpp); [H5] [PAM conversation](https://github.com/hyprwm/hyprlock/blob/main/src/auth/Pam.cpp); [H6] [license](https://github.com/hyprwm/hyprlock/blob/main/LICENSE); [H7] [latest-release API](https://api.github.com/repos/hyprwm/hyprlock/releases/latest); [H8] [fingerprint implementation](https://github.com/hyprwm/hyprlock/blob/main/src/auth/Fingerprint.cpp).
- [M1] [swaylock metadata](https://api.github.com/repos/swaywm/swaylock); [M2] [waylock mirror metadata](https://api.github.com/repos/ifreund/waylock); [M3] [gtklock metadata](https://api.github.com/repos/jovanlanik/gtklock); [M4] [hyprlock metadata](https://api.github.com/repos/hyprwm/hyprlock); [M5] [gtk-session-lock metadata](https://api.github.com/repos/Cu3PO42/gtk-session-lock).
- [A1] [Linux-PAM application developer guide](https://github.com/linux-pam/linux-pam/blob/master/doc/adg/Linux-PAM_ADG.xml); [A2] [polkit architecture / agents](https://www.freedesktop.org/software/polkit/docs/latest/polkit.8.html); [A3] [fprintd PAM README / separate-stack limitation](https://gitlab.freedesktop.org/libfprint/fprintd/-/blob/master/pam/README).
- [E1] [jirutka effects README](https://github.com/jirutka/swaylock-effects/blob/master/README.md) and [manifest](https://github.com/jirutka/swaylock-effects/blob/master/meson.build); [E2] [jirutka metadata: unmaintained](https://api.github.com/repos/jirutka/swaylock-effects); [E3] [mortie metadata](https://api.github.com/repos/mortie/swaylock-effects); [E4] [superty fork README](https://github.com/superty/swaylock-effects/blob/master/README.md) and [metadata](https://api.github.com/repos/superty/swaylock-effects); [E5] [marzzzello metadata](https://api.github.com/repos/marzzzello/swaylock-effects).
- [R1] [swaywardlock README / beta status / license](https://github.com/swayward-wm/swaywardlock/blob/main/README.md); [R2] [metadata](https://api.github.com/repos/swayward-wm/swaywardlock); [R3] [Amorphis README / early-stage warning](https://github.com/dani-77/amorphis/blob/main/README.md); [R4] [metadata](https://api.github.com/repos/dani-77/amorphis); [R5] [fransac wlock README](https://github.com/fransac/wlock/blob/main/README.md) and [shadow/crypt implementation](https://github.com/fransac/wlock/blob/main/wlock.c); [R6] [edzdez wlock README](https://github.com/edzdez/wlock/blob/main/README.md); [R7] [experimental Rust wlock metadata](https://api.github.com/repos/rottencandy/wlock); [R8] [Rust manifest](https://github.com/rottencandy/wlock/blob/main/Cargo.toml).
- [D1] [KScreenLocker build/dependencies](https://github.com/KDE/kscreenlocker/blob/master/CMakeLists.txt); [D2] [greeter Wayland layer-shell](https://github.com/KDE/kscreenlocker/blob/master/greeter/greeterapp.cpp); [D3] [compositor socket integration](https://github.com/KDE/kscreenlocker/blob/master/ksldapp.cpp); [D4] [per-file licenses](https://github.com/KDE/kscreenlocker/tree/master/LICENSES); [D5] [GNOME ScreenShield implementation](https://github.com/GNOME/gnome-shell/blob/main/js/ui/screenShield.js); [D6] [GNOME manifest / Mutter coupling](https://github.com/GNOME/gnome-shell/blob/main/meson.build); [D7] [COSMIC dispatch](https://github.com/pop-os/cosmic-greeter/blob/master/src/main.rs); [D8] [COSMIC lock / PAM implementation](https://github.com/pop-os/cosmic-greeter/blob/master/src/locker.rs); [D9] [dependencies](https://github.com/pop-os/cosmic-greeter/blob/master/Cargo.toml); [D10] [license](https://github.com/pop-os/cosmic-greeter/blob/master/LICENSE).
- [X1] [slock upstream](https://tools.suckless.org/slock/); [X2] [i3lock upstream](https://github.com/i3/i3lock); [X3] [xsecurelock upstream](https://github.com/google/xsecurelock); [X4] [XScreenSaver upstream](https://www.jwz.org/xscreensaver/).
- [I1] [swayidle README / idle protocol](https://github.com/swaywm/swayidle/blob/master/README.md); [I2] [swayidle manual / suspend readiness](https://github.com/swaywm/swayidle/blob/master/swayidle.1.scd); [I3] [loginctl lock-session manual](https://www.freedesktop.org/software/systemd/man/latest/loginctl.html#lock-session%20%5BID%E2%80%A6%5D); [I4] [wayidle upstream](https://github.com/softwaresale/wayidle/blob/main/README.md) and [actual idle protocol binding](https://github.com/softwaresale/wayidle/blob/main/src/app.rs).

- [Q1] [Quickshell session-lock API and semantics](https://github.com/quickshell-mirror/quickshell/blob/master/src/wayland/session_lock.hpp); [Q2] [Quickshell license](https://github.com/quickshell-mirror/quickshell/blob/master/LICENSE).

- [N1] [Smithay 0.7.0 session-lock module and confirmation contract](https://docs.rs/smithay/0.7.0/smithay/wayland/session_lock/index.html); [N2] Navi at the reviewed base commit: [manifest](https://github.com/Yoshiofthewire/navi/blob/967ba987619506c65e5aec6af6802dfcc77c5e9f/Cargo.toml), [frontend and focus](https://github.com/Yoshiofthewire/navi/blob/967ba987619506c65e5aec6af6802dfcc77c5e9f/src/state.rs), [input shortcuts](https://github.com/Yoshiofthewire/navi/blob/967ba987619506c65e5aec6af6802dfcc77c5e9f/src/input.rs), [session actions](https://github.com/Yoshiofthewire/navi/blob/967ba987619506c65e5aec6af6802dfcc77c5e9f/src/bin/navi-bar/session.rs), [private bar](https://github.com/Yoshiofthewire/navi/blob/967ba987619506c65e5aec6af6802dfcc77c5e9f/src/shell.rs).
- [N3] Smithay 0.7.0 [compositor defaults](https://docs.rs/smithay/0.7.0/smithay/wayland/compositor/struct.CompositorState.html), [output globals](https://docs.rs/smithay/0.7.0/smithay/output/struct.Output.html), [viewporter](https://docs.rs/smithay/0.7.0/smithay/wayland/viewporter/index.html), [single-pixel buffers](https://docs.rs/smithay/0.7.0/smithay/wayland/single_pixel_buffer/index.html), [idle notification](https://docs.rs/smithay/0.7.0/smithay/wayland/idle_notify/index.html); [N4] [Navi performance measurements](performance.md).
- [L1] [systemd login1 methods, signals and inhibitors](https://www.freedesktop.org/software/systemd/man/latest/org.freedesktop.login1.html); [L2] [systemd inhibitor-lock semantics and limits](https://systemd.io/INHIBITOR_LOCKS/).

- [SS1] [RFC 7643: SCIM core schema, identifiers and administrative status](https://www.rfc-editor.org/rfc/rfc7643.html); [SS2] [RFC 7644: SCIM protocol operations](https://www.rfc-editor.org/rfc/rfc7644.html); [SS3] [OpenID Connect Core: authentication, max_age and auth_time](https://openid.net/specs/openid-connect-core-1_0.html); [SS4] [OASIS SAML 2.0 assertions and protocols](https://docs.oasis-open.org/security/saml/v2.0/saml-core-2.0-os.pdf); [SS5] [RFC 8628: OAuth device authorization](https://www.rfc-editor.org/rfc/rfc8628.html).
- [SS6] GNU C Library [NSS](https://sourceware.org/glibc/manual/latest/html_node/Name-Service-Switch.html) and [user record fields](https://sourceware.org/glibc/manual/latest/html_node/User-Data-Structure.html); [SS7] Linux-PAM [session opening](https://github.com/linux-pam/linux-pam/blob/master/doc/man/pam_open_session.3.xml) and [pam_mkhomedir](https://github.com/linux-pam/linux-pam/blob/master/modules/pam_mkhomedir/pam_mkhomedir.8.xml); [SS8] [authd onboarding and account/home creation](https://documentation.ubuntu.com/authd/stable/howto/onboard-authd-user/); [SS9] authd [security/offline access/UID reuse](https://documentation.ubuntu.com/authd/stable/explanation/security/) and [offboarding](https://documentation.ubuntu.com/authd/stable/howto/offboard-authd-user/).
- [SS10] authd [architecture and capability negotiation](https://documentation.ubuntu.com/authd/stable/explanation/authd-architecture/) and [NSS user management](https://documentation.ubuntu.com/authd/stable/explanation/user-management/); [SS11] [authd GDM login flow](https://documentation.ubuntu.com/authd/stable/howto/login-gdm/); [SS12] authd [supported providers](https://documentation.ubuntu.com/authd/stable/reference/identity-providers/) and [provider-specific authentication flows](https://documentation.ubuntu.com/authd/stable/reference/authentication-flows/).
- [SS13] [SSSD upstream NSS/PAM introduction](https://github.com/SSSD/sssd/blob/master/README.md); [SS14] [current upstream SSSD IdP provider requirements](https://github.com/SSSD/sssd/blob/master/src/man/sssd-idp.5.xml); [SS15] [gtklock authentication conversation](https://github.com/jovanlanik/gtklock/blob/master/src/auth.c); [SS16] [Linux-PAM account management](https://github.com/linux-pam/linux-pam/blob/master/doc/man/pam_acct_mgmt.3.xml); [SS17] [RFC 6749 section 1.5: refresh tokens](https://www.rfc-editor.org/rfc/rfc6749.html#section-1.5).
