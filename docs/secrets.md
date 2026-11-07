# Secrets: oo7 and libsecret

Navi's selected vault is **oo7**. Applications use **libsecret** over the standard
`org.freedesktop.secrets` session-bus API; Navi does not implement encryption or put
passwords in desktop JSON, process arguments or logs. The service is separate from the
compositor and native Settings application. libsecret is a client library, not a second
vault service. [1][2]

Research checked on 2026-11-07 against oo7 **0.7.0**, commit
`989e1c1ddf4fb36c98548a86c39a96afa5f25b52`. This is newer than the oo7 client-source
packages currently listed for Ubuntu 26.04/26.10. Version 0.7.0 also supports unencrypted
keyrings; Navi must not select an empty-password/plaintext fallback. [1][3][4]

## Integration contract

- Start the installed daemon only for a full Navi session. A nested Navi must leave the
  host's Secret Service alone. Check existing ownership before spawning; never use
  `--replace`, stop another provider, or migrate existing vaults automatically.
- Locate `oo7-daemon` in the distro's libexec directory (`/usr/lib/oo7-daemon` on Arch,
  or `/usr/libexec/oo7-daemon` with upstream defaults). The normal launch has no password
  arguments and no `--login`: upstream PAM or a systemd credential supplies unlock data.
- The daemon Navi starts uses `$XDG_DATA_HOME/navi/secrets` (default `~/.local/share/navi/secrets`) as its data home; keyrings are beneath `keyrings/`. This isolates startup from legacy vault discovery. A preexisting oo7 service keeps its existing data location.
- Keep a started child alive for the session; stop only that owned child on logout.
  A preexisting service is shared user-session state and must survive Navi's exit.
- Verify the selected oo7 process/unique D-Bus owner before a Navi credential write.
  Bind the libsecret client to that owner so a provider-name change cannot redirect the
  write. If oo7 or libsecret is unavailable, reject a requested protected save clearly.
- First-run keyring creation and later unlocking remain with oo7's native prompter or
  PAM. Navi never asks for the login password and never installs or rewrites PAM rules.

The upstream daemon initializes its keyring/PAM listener before acquiring the well-known
name. Omitting `--replace` prevents name takeover, but is not a substitute for an ownership
preflight. oo7's normal startup can discover/migrate supported legacy stores; tests must
use private data roots, and Navi must not run a migration command on existing user data. [5]

## Installation and unlocking

On Arch, the official `oo7` package is 0.7.0-1 and includes the daemon, login helper,
`oo7-cli`, PAM module and optional Secret portal. Install `libsecret` for application
clients and `gcr` for graphical unlock prompts outside Plasma. **`gcr-4` does not supply
`gcr-prompter`; the Arch `gcr` package does.** [4][6]

```sh
sudo pacman -S oo7 libsecret gcr network-manager-applet
```

On Ubuntu, `libsecret-1-0`, `libsecret-tools`, `gir1.2-secret-1`, `network-manager-gnome` and `gcr` provide the client, `secret-tool`
and graphical prompter. The official package-name search currently lists only
`librust-oo7-dev`, which is Rust library source, not an installed oo7 daemon. Use a trusted
distro/backport package of the daemon or package the pinned upstream release; do not
assume `apt install oo7` exists. Ubuntu's GCR prompter is `/usr/libexec/gcr-prompter`. [3][7]

oo7 exposes a default Login collection. Without unlock material, it remains locked until
a supported prompt obtains a password. Graphical non-Plasma clients use GCR's
`org.gnome.keyring.SystemPrompter`; terminal `oo7-cli` commands export a temporary CLI
prompter. `secret-tool` does not supply that terminal prompter. Install and validate the
native GCR service rather than collecting passwords inside Navi. [5][8]

Automatic login unlocking uses `pam_oo7.so` and the packaged `oo7-daemon-login` helper.
Upstream documents optional `auth`, `session ... auto_start` and `password` hooks for
capture/unlock/password changes. The actual display-manager stack and ordering require
distro review; in particular, the runtime directory/session must be established before
starting user helpers. These are not changes for an unattended desktop install. The
server can also import the encrypted systemd credential
`oo7.keyring-encryption-password` with systemd 258 or newer. [9][10]

SCIM account provisioning remains independent of the vault. Browser/passwordless SSO may
not yield a reusable PAM password, so it does not automatically unlock a password-encrypted
vault. Keep explicit vault unlocking available; do not derive a key from an access token.

The upstream systemd unit is `oo7-daemon.service`, with a
`dbus-org.freedesktop.secrets.service` alias and `WantedBy=default.target`. The Arch
package has no separate `org.freedesktop.secrets.service` activation file. Globally enabling
that user unit can affect every desktop for the user; Navi-specific session startup must
not silently replace the user's KWallet/gnome-keyring choice in other sessions. [4][10]

## Newly saved Wi-Fi passwords

NetworkManager distinguishes system-owned secrets (`flags=0`), user-agent-owned secrets
(`flags=1`) and unsaved secrets (`flags=2`). Starting oo7 alone does not move a Wi-Fi
password into it. Existing saved profiles must retain their ownership and contents. [11]

For a newly remembered protected network, the design is:

1. Allocate a fresh connection UUID and verify that selected oo7 storage and the resident
   NetworkManager agent process is present. NetworkManager itself decides when registration is ready.
2. Store the submitted password through the owner-bound libsecret service using
   NetworkManager's standard schema/attributes.
3. Send an agent-owned profile to NetworkManager with no embedded password. Its agent
   retrieves the saved secret from the vault. If vault storage fails, do not create the
   profile. Existing user entries are never deleted. A failed or timed-out activation
   does not prove that profile creation was rolled back; preserve staged credentials
   unless the implementation can establish that this new profile was never created.

The mature bridge is `nm-applet` from `network-manager-applet`, which depends on libsecret.
The inspected 1.36-era source uses schema `org.freedesktop.NetworkManager.Connection`
with `SECRET_SCHEMA_DONT_MATCH_NAME` and these string attributes: [12][13]

- `connection-uuid`: the profile UUID.
- `setting-name`: `802-11-wireless-security` for PSK/SAE, or `802-1x` for PEAP.
- `setting-key`: `psk` or `password`, respectively.

Its lookup searches UUID + setting name and obtains the setting key from each item.
`SaveSecrets` stores only secrets whose flags equal `AGENT_OWNED`. Its callback currently
ignores libsecret store errors, so successful NetworkManager profile creation does **not**
prove that a password reached the vault. The explicit libsecret pre-store is necessary. [12]

Do not claim `nm-applet --no-icon` disables its tray: the inspected parser has `--indicator`
and `--no-agent`, but **no `--no-icon` option**; unknown arguments are ignored. A desktop
without a tray may show no icon, while the applet's agent still runs. Its GApplication
singleton is `org.freedesktop.network-manager-applet`, and its NetworkManager agent
identifier is `org.freedesktop.nm-applet`. [12][13]

The singleton's presence is not an agent-registration acknowledgement. NetworkManager
registration occurs later; libnm documents waiting for successful registration explicitly.
The applet exposes no public registration-ready D-Bus method. NetworkManager can select
another eligible agent or try a later agent after an error. The external applet also
performs its lookups against the default Secret Service; owner-binding Navi's write does
not pin all later applet lookups if another provider replaces that bus name. These limits
must be reflected in readiness checks and activation-failure handling. [14][15]

This bridge covers new remembered Wi-Fi PSK/SAE/PEAP passwords. It is not an automatic
migration of existing profiles or every VPN/WWAN credential, and it must not change
"do not remember" into persistent storage.

## Isolated verification

Run the real installed daemon/client check from the checkout:

```sh
python3 scripts/check-secrets.py --daemon /usr/lib/oo7-daemon --output /tmp/navi-secrets-check
# Use /usr/libexec/oo7-daemon on distributions with upstream's libexec layout.
```

The test must use a new disposable output directory. It uses `dbus-run-session`, private
XDG directories and `OO7_PAM_SOCKET` pointing into that private runtime. Launch the daemon with `--login`,
passing a generated, nonempty dummy vault password over a subprocess stdin pipe. This
bypasses the normal login-helper lookup, whose default path in 0.7.0 is hard-coded to
`/run/user/<uid>/oo7-daemon-login.sock` even with a private `XDG_RUNTIME_DIR`. Never feed a
real login password or inherit its helper in tests. [5]

Exercise the installed libsecret client, not a mock cipher: store a generated dummy item
with `secret-tool store` using stdin, capture/compare `secret-tool lookup` without printing
the value, restart oo7 against the same private data/password and repeat lookup, then
clear the item. Assert that its bytes do not appear in the persisted keyring. Also check
locked collections, missing dependencies, an existing provider, process exit and bounded
startup failures. Do not test encryption by selecting oo7's unencrypted format.

The native-agent check uses the actual applet with a synthetic NetworkManager on the private
bus and its own Xvfb display. Upstream libnm supports `LIBNM_USE_SESSION_BUS=1` for this fixture;
in that mode it explicitly permits a non-root fake NetworkManager. That switch is set only
for test children and removed from production helpers. No host system bus, real network or
root authentication workaround is used. [14][16]

```sh
bash scripts/build-oo7-test.sh
python3 scripts/check-secrets.py --daemon "${CARGO_TARGET_DIR:-target}/debug/oo7-daemon" \
  --nm-applet /usr/bin/nm-applet --xvfb /usr/bin/Xvfb --output /tmp/navi-secret-agent-check
```

The build script is test infrastructure: it verifies a SHA-256-pinned upstream archive,
uses the existing Cargo target/cache and installs no provider. CI runs this real-service check.
Verified locally with oo7 0.7.0 and native nm-applet 1.36.0: encrypted store/read/delete,
restart persistence, wrong master-password rejection, WPA2/WPA3/enterprise password retrieval and native-agent deletion,
agent-owned profiles without password fields, failed vault saves before any NM profile
change, sanitized credential errors, and refusal of a foreign provider.

Graphical prompts, PAM login/password changes and real reconnect remain separate
supervised checks until executed. A libsecret roundtrip against an already-unlocked test
vault alone does not establish those behaviors.

## Primary sources

[1]: https://github.com/linux-credentials/oo7/releases/tag/0.7.0
[2]: https://gnome.pages.gitlab.gnome.org/libsecret/
[3]: https://packages.ubuntu.com/search?keywords=oo7&searchon=names&suite=all&section=all
[4]: https://archlinux.org/packages/extra/x86_64/oo7/files/
[5]: https://github.com/linux-credentials/oo7/blob/989e1c1ddf4fb36c98548a86c39a96afa5f25b52/server/src/main.rs
[6]: https://archlinux.org/packages/extra/x86_64/gcr/files/
[7]: https://packages.ubuntu.com/stonking/amd64/gcr/filelist
[8]: https://github.com/linux-credentials/oo7/blob/989e1c1ddf4fb36c98548a86c39a96afa5f25b52/server/src/prompt/mod.rs
[9]: https://github.com/linux-credentials/oo7/blob/989e1c1ddf4fb36c98548a86c39a96afa5f25b52/pam/README.md
[10]: https://github.com/linux-credentials/oo7/blob/989e1c1ddf4fb36c98548a86c39a96afa5f25b52/server/data/oo7-daemon.service.in
[11]: https://networkmanager.dev/docs/api/latest/secrets-flags.html
[12]: https://github.com/NetworkManager/network-manager-applet/blob/f155a0d852173daec048b79184b91943bca6b8ae/src/applet-agent.c
[13]: https://github.com/NetworkManager/network-manager-applet/blob/f155a0d852173daec048b79184b91943bca6b8ae/src/main.c
[14]: https://github.com/NetworkManager/NetworkManager/blob/4fe2d881f81a000d906b614bab64f629faa2b169/src/libnm-client-impl/nm-secret-agent-old.c
[15]: https://github.com/NetworkManager/NetworkManager/blob/4fe2d881f81a000d906b614bab64f629faa2b169/src/core/settings/nm-agent-manager.c

[16]: https://github.com/NetworkManager/NetworkManager/blob/4fe2d881f81a000d906b614bab64f629faa2b169/src/libnm-client-impl/tests/test-secret-agent.c#L714
