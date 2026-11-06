# UnifiedPush integration research

Investigated 2026-11-06 against the official specification and KDE source. These are
implementation findings, not a claim that Navi already implements the controls below.

## Recommended architecture

Use the optional, existing `kunifiedpush-distributor` process for push transport. Keep it
outside the compositor and start it only after the user enables UnifiedPush in a full Navi
session. The installed `kunifiedpush` package on Usagi is `26.08.2-1.1` and provides
`/usr/bin/kunifiedpush-distributor`. No additional network or cryptography library is needed
in Navi. KDE supports Ntfy, Mozilla Autopush, Gotify and NextPush providers. [1][2]

UnifiedPush is a transport between a provider and participating applications. The distributor
forwards opaque bytes to each application's connector; that application decrypts/interprets
the message and may call the desktop notification server. It is not a generic JSON-to-popup
protocol. A Navi distributor must not turn arbitrary push payloads into notification text.
The current spec requires encrypted RFC 8291 payloads; applications own their encryption keys.
An application server must support Web Push, or use a gateway appropriate to its protocol. [3][4]

For the requested default-off behavior:

- Persist a Navi preference defaulting to `false`. Opening Notifications must not start a
  distributor, enable a user service, register an endpoint or contact a provider.
- In a full session, supervise a dedicated child process with a private configuration root,
  such as `$XDG_CONFIG_HOME/navi/unifiedpush`, passed as the child's `XDG_CONFIG_HOME`.
  The Qt settings file then lives beneath that root at `KDE/kunifiedpush-distributor.conf`.
  Use mode 0700 for private directories and 0600 for configurations containing credentials or
  registration capabilities. Leave existing KDE configuration untouched.
- Stop only the owned child when disabled or when Navi exits. Do not enable/disable the
  distro's pre-enabled `kunifiedpush-distributor.service`, stop a preexisting process, or claim
  that Navi's toggle controls a separately running distributor.
- In nested Navi, disable lifecycle and provider mutations with a clear explanation. Testing
  can exercise the complete daemon on a private D-Bus session instead.
- Check for a preexisting owner before launch and handle a start race by inspecting child
  exit. Do not replace the existing D-Bus name owner.
- Report missing executable, disabled, starting, idle, connected and provider errors accurately.
  Merely owning the D-Bus name does not mean a server connection exists.

These lifecycle and isolation rules are Navi integration recommendations. KDE's stock user
unit belongs to `graphical-session.target` and uses the same well-known name, so modifying it
would also affect other desktops. [5]

### Version constraint

The installed **v26.08.2 hardcodes** `org.unifiedpush.Distributor.kde`. It cannot run alongside
another daemon that owns that name on the same bus. Newer KDE `master` permits a custom
suffix through `KDE_DISTRIBUTOR_SERVICE_NAME` (for example `navi` gives
`org.unifiedpush.Distributor.navi`), but assuming this works on v26.08.2 would silently fail.
Keep compatibility with the installed version, or explicitly require a version containing the
override. [6][7]

Do not unconditionally set `UNIFIEDPUSH_DISTRIBUTOR`: the current specification expects a full
bus name, while KDE v26.08.2's connector utility expects a suffix such as `kde`. With a single
distributor, normal discovery avoids this version mismatch. [3][8]

## Native Settings management API

Destination: `org.unifiedpush.Distributor.kde` on the session bus. Path: `/Management`.
Interface: `org.kde.kunifiedpush.Management`. The installed version exposes these methods: [9][10]

| Method | Input | Output / meaning |
|---|---|---|
| `status` | none | `i`: 0 Unknown, 1 Idle, 2 Connected, 3 NoNetwork, 4 AuthenticationError, 5 NoSetup |
| `errorMessage` | none | `s`: provider error; treat as untrusted bounded display text |
| `pushProviderId` | none | `s`: `Ntfy`, `Autopush`, `Gotify` or `NextPush` |
| `pushProviderConfiguration` | `s` provider ID | `a{sv}` configuration |
| `setPushProvider` | `s`, `a{sv}` | no reply; can unregister/re-register clients and change endpoints |
| `registeredClients` | none | `a(sss)`: connection token, application bus service, description |
| `forceUnregisterClient` | `s` connection token | no reply; explicit user action only |

Signals: `statusChanged`, `errorMessageChanged`, `pushProviderChanged`, and
`registeredClientsChanged`. Re-read values after the matching signal. Native GTK/Gio can call
the API without Qt UI or a new Python dependency. Keep blocking calls off GTK's main thread.
Use `NO_AUTO_START` for inspection so merely viewing Settings cannot start a service.

The tokens returned by `registeredClients` are capabilities. Do not put them in labels,
logs or diagnostics. Show the application service/description, retaining the token only for
the explicitly confirmed removal operation. Provider changes can rotate endpoints; expose
that consequence before applying. [9][11]

### Configuration

KDE uses QSettings with the following groups; an unauthenticated Ntfy provider is the smallest
server configuration. Require an explicit HTTPS URL in Navi, retaining normal TLS certificate
verification. KDE's network manager uses `NoLessSafeRedirectPolicy` and HSTS. [1][12]

```ini
[PushProvider]
Type=Ntfy

[Ntfy]
Url=https://ntfy.sh
```

Other supported provider fields: [1][13]

- `Autopush`: `Url=https://push.services.mozilla.com`.
- `Gotify`: `Url`, `ClientToken`.
- `NextPush`: `Url`, `Username`, `AppPassword`.
- Ntfy in the installed v26.08.2 supports `AuthMethod` (`None`, `Basic`, `Bearer`), `Username`
  and `Secret`. [14]

Autopush accepts `Url` with the documented `https://push.services.mozilla.com` value: the
provider reads this key and converts `https` to `wss` (or `http` to `ws`) when opening its
WebSocket. Do not change the stored scheme to `wss`; v26.08.2 explicitly expects HTTP(S). [17]

Do not copy a system provider URL implicitly: this host's distro config defaults to
`https://unifiedpush.kde.org/`, while the user may prefer a self-hosted server. Provider choice
should be visible before opt-in. Do not send passwords in process arguments or logs.

### Preserve registrations and provider identity

**Do not rewrite the whole QSettings file when enabling or restarting.** The same
`KDE/kunifiedpush-distributor.conf` stores user-facing provider configuration and all of the
following durable state: [11][14][16][17][18]

- `[Clients] Tokens`: list of registered connection tokens.
- `[<connection-token>]`: `RemoteId`, `ServiceName`, `Endpoint`, `Description`, `VAPIDKey`,
  and `Version` for that registration.
- `[Ntfy-internal] Topics` and `LastMessageId`: subscribed topics and replay cursor.
- `[Autopush-internal] UAID`: provider-side user-agent identity.
- `[NextPush] DeviceId`: provider-side device identity.

Replacing the file with only `[PushProvider] Type` and `[Ntfy] Url` loses endpoints and
subscriptions on the next enable, restart or session login. Retaining client records but
changing only the provider URL is also incorrect: the saved provider identity and remote
registrations belong to the original server.

Initialize a new private configuration only when it does not yet exist. For an unchanged
provider/URL, leave its bytes untouched. For deliberate provider changes, use the owned daemon's
`Management.setPushProvider`: KDE unregisters old registrations, changes the provider, resets
provider state and re-registers the existing applications, notifying each of its new endpoint.
Explain endpoint rotation before applying. Preserve an unreadable or malformed existing file
and report the problem rather than replacing it. [11]

Stopping and restarting the owned process must retain the private configuration directory.
Prefer graceful termination with a bounded timeout before force-kill, to allow outstanding
provider/configuration writes to finish. Never remove this state as a side effect of turning
delivery off.

### XDG isolation and application discovery

It is safe to give the distributor private `XDG_CONFIG_HOME`, `XDG_DATA_HOME`,
`XDG_STATE_HOME` and `XDG_CACHE_HOME` paths. Keep `DBUS_SESSION_BUS_ADDRESS` and
`XDG_RUNTIME_DIR` for the real session unchanged. KDE's durable registration/provider identity
uses QSettings under the private config root; HSTS state uses Qt's GenericCacheLocation beneath
the private cache root. No additional durable identity file was found under data/state in the
inspected v26.08.2 provider implementations. [11][12][14][16][17][18]

Connector discovery/activation does not scan the distributor child's own data directory:
`purgeUnavailableClients` calls the existing session bus's `ListActivatableNames`, and
`Client::activate` asks that bus to start the connector. The bus daemon has its own service
search paths/environment. Overriding `XDG_DATA_HOME` only in the distributor child therefore
does not hide application activation files already known to the bus. This was verified with
an activatable connector in one private bus-data root and a distinct distributor-child data
root, including a distributor restart. Do not replace the application's own XDG environment
or the running bus daemon's activation environment. [11][16]

## Standard application-facing D-Bus protocol

The official spec currently identifies itself as **DBUS_0.3.0**, modified 2026-09-24. The KDE
daemon exposes both v1 and v2; new client code should follow v2. [3][15]

- Distributor bus names start with `org.unifiedpush.Distributor.`. Object path
  `/org/unifiedpush/Distributor`, interface `org.unifiedpush.Distributor2`.
- Connector bus name identifies its application in reverse DNS form. Object path
  `/org/unifiedpush/Connector`, interface `org.unifiedpush.Connector2`.
- Every method takes one `a{sv}` and returns one `a{sv}`. Invalid fields must be ignored.
- `Distributor2.Register`: required string fields `service`, `token`; optional `description`,
  `vapid`. Returns `success` = `REGISTRATION_SUCCEEDED` or `REGISTRATION_FAILED`; failure also
  supplies `reason` = `INTERNAL_ERROR`, `NETWORK`, `ACTION_REQUIRED`, `VAPID_REQUIRED` or
  `UNAUTHORIZED`. Successful registration delivers the endpoint through `NewEndpoint`.
- `Distributor2.Unregister`: `token`; returns an empty dictionary.
- `Connector2.NewEndpoint`: `token`, `endpoint`; returns an empty dictionary.
- `Connector2.Message`: `token`, `message` (`ay`), optional `id`; return the same `id` when
  supplied. The distributor must not wait for connector-method responses.
- `Connector2.Unregistered`: `token`; returns an empty dictionary.
- Connection tokens are randomly generated by the application, unique and unguessable;
  maximum 100 bytes. UUIDv4 is suggested. Unknown tokens are ignored.
- Descriptions are at most 100 bytes; endpoint URLs at most 1000 bytes; VAPID public keys are
  87-byte base64url P-256 keys. Push bodies are raw POST data, 1–4096 bytes, encrypted per
  RFC 8291. Endpoints are capability URLs, so keep them secret. [3][15]

Applications discover running/activatable distributors, choose one, reuse their token and
register at each startup to refresh the endpoint. They normally provide a matching D-Bus
activation `.service` file so receiving push does not open a window and can wake the app.
KDE purges registrations whose connector service is no longer activatable. [3][11]

## Verification performed

Ran the installed daemon on a fresh `dbus-run-session` with private configuration and
`[PushProvider] Type=Mock`. The system bus address was made unavailable. Verified real v2
interface introspection, idle status, successful registration, management client listing and
unregistration. The owned daemon exited afterward. No network provider, real system service,
user configuration or existing daemon was modified.

Evidence on Usagi: `/tmp/navi-unifiedpush-research/verify.py` and
`/tmp/navi-unifiedpush-research/isolated-v29txrwx/daemon.log`.

Also ran a local receive-pipeline check with a private bus, a loopback fake Ntfy SSE provider,
the real installed distributor and a real Gio `Connector2` application object. Verified actual
`NewEndpoint` and `Message` callbacks, byte-preserving opaque payload delivery (including
zero/non-UTF-8 bytes), retained registered clients after restart,
and the same endpoint/topic across that restart. The child used separate private config,
data, state and cache roots; the connector's activation file was known only to the bus daemon's
data root. This establishes local transport, persistence and discovery behavior with the
installed binary. It does not establish encryption or public-provider behavior.

The built-in Mock provider has no public network/D-Bus injection API for a push message; its
source only emits connect/register/unregister/ack callbacks. A loopback SSE fixture therefore
exercises the actual receive path more fully than a Mock-only registration check. [19]

Additional evidence: `/tmp/navi-unifiedpush-research/receive.py` and
`/tmp/navi-unifiedpush-research/receive-z39pyqp1/results.json`.

The repeatable repository check includes those assertions plus a live
`Management.setPushProvider` URL change and unregistration:

```sh
python3 scripts/check-unifiedpush.py
# Optional: retain configuration, daemon log and JSON results in a NEW directory.
python3 scripts/check-unifiedpush.py --output /tmp/navi-unifiedpush-proof
```

It requires `dbus-run-session`, `kunifiedpush-distributor` and Python PyGObject/Gio. Missing
optional prerequisites return exit status 77. The script always creates its own bus and XDG
roots, blocks access to the system bus for its child, runs only a loopback HTTP fixture,
stops its own daemon, and removes temporary evidence unless `--output` is supplied. A separate
connector activation directory verifies that child-data isolation does not erase the bus's
view of installed connectors. It deliberately keeps fixture registration tokens local.

Recorded passing run: `/tmp/navi-unifiedpush-research/repo-proof/results.json`.

Ntfy v26.08.2 uses the SSE message ID as its persisted replay cursor but omits the optional
`id` field in its `Connector2.Message` call. The test echoes an ID if provided, but does not
claim message-ID acknowledgement coverage for this provider. [14]

Not verified: public-provider delivery, real TLS failures, application decryption or end-to-end
desktop popups. The local HTTP fixture is isolated test infrastructure; Navi's production
provider URL control should continue to require HTTPS. This external-daemon protocol check
does not exercise Navi's supervisor, PID ownership checks, default-off preference, nested
guard or Settings controls; those need their own runtime tests.

## Sources

1. [KDE README, v26.08.2](https://invent.kde.org/libraries/kunifiedpush/-/blob/v26.08.2/README.md).
2. [UnifiedPush Linux distributor guide](https://unifiedpush.org/users/distributors/kunifiedpush/).
3. [Official D-Bus specification](https://unifiedpush.org/developers/spec/dbus/).
4. [UnifiedPush architecture definitions](https://unifiedpush.org/developers/spec/definitions/).
5. [KDE user unit, v26.08.2](https://invent.kde.org/libraries/kunifiedpush/-/blob/v26.08.2/src/distributor/kunifiedpush-distributor.service.in).
6. [Daemon main, v26.08.2](https://invent.kde.org/libraries/kunifiedpush/-/blob/v26.08.2/src/distributor/main.cpp).
7. [Daemon main, master](https://invent.kde.org/libraries/kunifiedpush/-/blob/master/src/distributor/main.cpp).
8. [Connector discovery, v26.08.2](https://invent.kde.org/libraries/kunifiedpush/-/blob/v26.08.2/src/shared/connectorutils.cpp).
9. [Management XML, v26.08.2](https://invent.kde.org/libraries/kunifiedpush/-/blob/v26.08.2/src/interfaces/org.kde.kunifiedpush.distributor.Management.xml).
10. [Status enum, v26.08.2](https://invent.kde.org/libraries/kunifiedpush/-/blob/v26.08.2/src/shared/distributorstatus_p.h).
11. [Distributor implementation, v26.08.2](https://invent.kde.org/libraries/kunifiedpush/-/blob/v26.08.2/src/distributor/distributor.cpp).
12. [Network/TLS defaults, v26.08.2](https://invent.kde.org/libraries/kunifiedpush/-/blob/v26.08.2/src/distributor/abstractpushprovider.cpp).
13. [Client tuple, v26.08.2](https://invent.kde.org/libraries/kunifiedpush/-/blob/v26.08.2/src/shared/clientinfo_p.h).
14. [Ntfy implementation, v26.08.2](https://invent.kde.org/libraries/kunifiedpush/-/blob/v26.08.2/src/distributor/ntfypushprovider.cpp).
15. Official XML: [Distributor2](https://codeberg.org/UnifiedPush/specifications/src/branch/main/specifications/dbus/org.unifiedpush.Distributor2.xml),
    [Connector2](https://codeberg.org/UnifiedPush/specifications/src/branch/main/specifications/dbus/org.unifiedpush.Connector2.xml).
16. [Registration storage and connector calls, v26.08.2](https://invent.kde.org/libraries/kunifiedpush/-/blob/v26.08.2/src/distributor/client.cpp).
17. [Autopush identity storage, v26.08.2](https://invent.kde.org/libraries/kunifiedpush/-/blob/v26.08.2/src/distributor/autopushprovider.cpp).
18. [NextPush identity storage, v26.08.2](https://invent.kde.org/libraries/kunifiedpush/-/blob/v26.08.2/src/distributor/nextpushprovider.cpp).
19. [Mock provider, v26.08.2](https://invent.kde.org/libraries/kunifiedpush/-/blob/v26.08.2/src/distributor/mockpushprovider.cpp).
