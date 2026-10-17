# Private Horizon gateway

This Node 22 service exposes one private game and an owner-approved mod portal. It forwards authenticated `/game` WebSockets only to `127.0.0.1:<gamePort>`. The native protocol, packet compression and world simulation remain in Horizon. The gateway checks the initial offline login name and UUID against the signed-in browser session before opening the TCP connection.

`horizon-server --dir /srv/horizon/world run` is the supervisor command. It launches `horizon-client --dedicated --nogui`; `horizon-server --dedicated` is not a command. See [the supervisor CLI](../../crates/horizon-server/src/main.rs) and [engine launcher](../../crates/horizon-server/src/engine.rs).

## Local verification

```sh
cd web/server
npm ci --ignore-scripts
npm test
npm audit --omit=dev
```

The default tests run a real HTTP/WebSocket gateway and TCP upstream, with a deliberately synthetic validator subprocess. They cover session separation, origin/CSRF checks, private paths and symlinks, exact approval hashes, immutable artifacts, validation timeouts, server mirrors, revocation, concurrent approvals, identity binding, notifications and bounded uploads.

The opt-in native integration test uses the actual Wasmtime validator, Jco, `coords_hud.hmod` and a server-side guest. Set absolute executable paths; the package paths default to `target/mods-test/mods/coords_hud.hmod` and `target/mods-test/mods/server-side/test_guest.hmod`, or override `HORIZON_COORDS_PACKAGE` / `HORIZON_SERVER_PACKAGE`:

```sh
HORIZON_REAL_VALIDATOR=/absolute/horizon-mod-validator \
HORIZON_REAL_JCO=/absolute/web/mods/node_modules/.bin/jco \
node --test test/real-validator.test.mjs
```

This verifies real archive validation and fuel-limited initialization, transpilation, owner approval, every downloaded artifact's SHA-256, notification revisions, native mirror bytes, revocation and invalid-archive rejection under the production subprocess limits. It still does not establish two-browser gameplay or deployment performance; those require their own checks.

## Private configuration

Create a JSON file readable only by the service account (`0600`), outside every served directory. The CLI rejects broader file permissions. This is an illustrative configuration; replace paths and the example domain with the installed files and actual HTTPS origin:

```json
{
  "origin": "https://game.example.com",
  "listenHost": "127.0.0.1",
  "listenPort": 8080,
  "gamePort": 25565,
  "publicDir": "/srv/horizon/web/dist",
  "assetsDir": "/srv/horizon/assets",
  "modsRuntimeDir": "/srv/horizon/web/mods",
  "privateDir": "/var/lib/horizon-gateway",
  "validatorExecutable": "/srv/horizon/bin/horizon-mod-validator",
  "jcoExecutable": "/srv/horizon/web/node_modules/.bin/jco",
  "discord": {"clientId": "APPLICATION_ID", "clientSecret": "CLIENT_SECRET", "ownerIds": ["HOST_DISCORD_USER_ID"]},
  "adminHash": "REPLACE_WITH_DIFFERENT_SCRYPT_HASH",
  "protectedNames": ["OwnerPlayerName"],
  "trustLoopbackProxy": true,
  "maxConnections": 16,
  "maxSessions": 32,
  "maxCandidates": 32,
  "maxPublishedVersions": 128
}
```

`publicDir`, `assetsDir` and `modsRuntimeDir` must already exist. Roots are resolved through symlinks before checking that private state is separate. Static reads also check the real file path, rejecting hidden files, parent traversal and links outside their root. `/assets/` and `/mods-runtime/` require a friend session. Only the launcher files under `publicDir` are public.

Configure Discord's authorization-code flow with the exact callback `${origin}/auth/discord/callback`. Request only `identify`; store the application secret in this private configuration. Generate a separate owner secret with `node hash-secret.mjs` on standard input and put only its scrypt hash in `adminHash`. Never put plaintext secrets in command arguments, committed files or browser configuration.

Discord friend sessions use Secure, HttpOnly, SameSite=Strict cookies for 30 days. The one-time OAuth binding cookie uses SameSite=Lax so Discord's top-level callback can send it. Profiles and hashed session tokens persist atomically in private mode-0600 `accounts.json`; raw session tokens and Discord access tokens are not persisted. Owner sessions remain independent, twenty-minute, memory-only sessions. Every account must choose a case-insensitively unique, permanent IGN and a wardrobe skin or previewed Minecraft username link before joining or uploading. Skins can be changed later.

**`protectedNames` must include every operator name in the native server.** A matching name can only be claimed by `discord.ownerIds` or with an independently authenticated owner session. The native TCP server stays loopback-only. Installations without a `discord` configuration can still use the legacy `inviteHash` API; the hosted Discord launcher does not expose that flow. Configuring Discord disables legacy `/api/login`.

Cloud provisioning credentials are not part of this configuration, service environment, browser build, game world or public assets. The application never needs them. Provisioning, if performed, belongs in a separate operator process.

Start one gateway process per private directory:

```sh
node /srv/horizon/web/server/cli.mjs /etc/horizon/gateway.json
```

For development only, HTTP is accepted with `allowInsecureLocalhost: true` and a literal localhost origin. Production always requires HTTPS. Host, Origin and WebSocket destination-query checks are strict; the reverse proxy must preserve the public Host. `trustLoopbackProxy` accepts the last forwarded IP only when the connecting peer is loopback.

## HTTP and WebSocket interface

Every mutation requires the exact same Origin. Authenticated mutations also require `X-CSRF-Token` from the matching friend or owner session. JSON requests use `Content-Type: application/json`. Errors are `{ "error": "message" }`, with 401 for missing sign-in, 403 for origin/CSRF failure, 409 for a conflicting decision, 413 for size, and 429 for throttling.

| Endpoint | Role | Request / response |
|---|---|---|
| `GET /api/session` | Public | Profile status, IGN, skin, CSRF and expiry; no Discord ID or credentials |
| `GET /auth/discord` | Public | Start the Discord authorization-code flow |
| `GET /auth/discord/callback` | OAuth state + cookie | Consume the code and establish a remembered session |
| `GET /api/skins` | Public | Nine wardrobe IDs and local preview URLs |
| `POST /api/skins/minecraft` | Signed in + CSRF | `{name}` Minecraft Java IGN → `{skin}` live preview/link descriptor; six lookups/account/minute |
| `GET /player-skins/mc_<model>_<sha256>.png` | Signed in | Bounded, normalized immutable skin image; no arbitrary file paths |
| `POST /api/profile` | Signed in + CSRF | Claim a unique IGN once; choose/change an allowed skin |
| `GET /api/players/skins` | Complete profile | UUID-to-IGN/skin mapping, without Discord IDs |
| `POST /api/login` | Legacy only | Disabled (410) when Discord is configured |
| `POST /api/logout` | Friend | Revokes the session and closes its WebSockets |
| `GET /api/admin/session` | Public | `{authenticated,csrf,expires}` or `{authenticated:false}` |
| `POST /api/admin/login` | Public | `{secret}`; sets independent owner cookie |
| `POST /api/admin/logout` | Owner | Revokes the owner session |
| `POST /api/mods/upload` | Friend | Raw `.hmod`, `application/octet-stream`, maximum 16 MiB; 202 `{sha256,status:"pending"}` or 422 `{sha256,status:"rejected",error}` |
| `GET /api/mods/pending` | Owner | `{candidates:[{sha256,status,uploader,created,manifest,browser,checks,size,liveReload}]}` |
| `POST /api/mods/<sha>/approve` | Owner | `{sha256:"<reviewed SHA-256>"}`; publishes only that validated candidate |
| `POST /api/mods/<sha>/reject` | Owner | Same exact-hash body; rejects a pending candidate |
| `POST /api/mods/<sha>/revoke` | Owner | Same exact-hash body; withdraws that currently active version |
| `GET /mods/index.json` | Friend | `{schema:1,revision,mods:[...]}` |
| `GET /mods/packages/<sha>.hmod` | Friend | Previously approved immutable package |
| `GET /mods/bundles/<sha>/<path>` | Friend | Previously approved immutable browser artifact |
| WebSocket `/mods/events` | Friend | Server sends `{type:"mods-changed",revision}` on connect and index change; clients fetch/verify the index |
| WebSocket `/game` | Friend | Binary native protocol stream, fixed upstream, no query parameters |
| `GET /healthz` | Public | `{ok:true}` means gateway HTTP is available, not game readiness |

Decision responses are `{ok:true,revision}`. Approval is never automatic. The owner reviews the pending full manifest, validation checks, uploader and exact content digest. Active entries include the complete manifest as `manifest`, its fields at the top level, the package `sha256`, `size`, `url`, browser `entry`, and `files:[{path,url,sha256,size}]`. Server-only entries have `entry:null` and `files:[]`; browser clients skip those. Client and both-side browser bundles have their own per-file digests. A client must verify bytes before activating them and retain its last working instance if replacement initialization fails.

Approvals serialize index writes. Packages and generated files are copied under immutable digest paths, rechecked, fsynced, and referenced by an atomically replaced index. After that durable decision, server mirrors reconcile and clients receive a new revision. An interrupted mirror operation can make an approval return an error after the index committed; retrying that exact approval or restarting repairs the mirror without a new revision. Review annotations are secondary to the durable index. Old approved artifacts remain available so an in-flight reload can finish; revocation removes the active entry and triggers unloading, but is not deletion of previously downloaded bytes.

Pending payloads consume bounded staging capacity. Rejected and published candidate payloads are removed while their review metadata remains. Published history is capped at `maxPublishedVersions` (default 128); it is not silently evicted. Archive old releases during planned maintenance, keeping hashes still used by clients. Registry/content mods are validated but marked `liveReload:false` and cannot be approved here: changing baked content requires a coordinated content release.

At most eight client/both mods may be active, matching the browser host's Worker budget. Server-only mods do not consume those eight slots. The entire active index has a hard ceiling of 32 entries (`maxActiveMods` can set a lower total). A new ID or server-to-browser side change exceeding a limit returns 409 until an active mod is revoked; updating an existing browser mod does not consume another slot. These limits bound declared guest memory per module but do not establish total browser or native-server memory use.

## Native validator and server mirror

The gateway invokes one native validator at a time, without a shell:

```text
/absolute/horizon-mod-validator --input /private/candidate/package.hmod --browser-out /private/candidate/browser
```

Only a zero exit and `ok:true` JSON on stdout are accepted:

```json
{
  "ok": true,
  "manifest": {"id":"coords_hud","name":"Coordinates HUD","version":"1.0.0","api":"0.1","side":"client","capabilities":["game-read","hud"],"description":"","authors":[]},
  "browser": {"entry":"mod.js","files":[{"path":"mod.js","sha256":"64 lowercase hexadecimal characters","size":123}]},
  "checks": {"manifest":"passed","imports":"passed","content":"passed","init":"passed"}
}
```

`browser` is null for server-only mods. The native validator owns archive/content bounds, `Manifest::parse`, import/capability checks and Wasmtime fuel-limited initialization. The gateway independently checks package and generated-file hashes, safe relative filenames, no symlinks, at most 64 generated files, 16 MiB per file and 32 MiB total generated output. It rejects output/diagnostic streams exceeding 256 KiB and kills the process group after thirty seconds. Linux `prlimit` adds twenty CPU seconds, 128 MiB per generated file, 128 open descriptors and no core dumps. Its environment contains only a fixed PATH, locale and configured Jco path; service/cloud secrets are not inherited. `validatorResourceLimits:false` is for tests, not deployment.

Set the native engine's `HORIZON_APPROVED_MODS` to `<privateDir>/approved-server`. Only approved `server`/`both` code packages appear there as `<id>.hmod`, copied and renamed atomically. Revocation removes that file. On gateway startup, reconciliation restores missing approved files and removes withdrawn files. The engine must read this mirror, compare content hashes, initialize a candidate before swapping, and retain its last working instance on failure. Native host capabilities remain defined by Horizon's WIT; approval does not add arbitrary filesystem/network/process access.

## Host boundaries and resource limits

Bind the native server to `127.0.0.1` in `server.toml` (`[server] bind = "127.0.0.1"`) and keep its port equal to `gamePort`. The supervisor configuration is described in [server documentation](../../docs/server.md). Run the native engine and gateway as an unprivileged service identity with access only to the required world/assets/private paths. The private mirror must be readable by the native engine; do not make it web-accessible to solve file permissions.

A TLS proxy can forward the one origin to the gateway:

```caddyfile
game.example.com {
  reverse_proxy 127.0.0.1:8080
}
```

Caddy supports WebSocket upgrade forwarding through [`reverse_proxy`](https://caddyserver.com/docs/caddyfile/directives/reverse_proxy). Keep ports 8080, 25565 and native administration closed publicly. Only the TLS entry and authorized operator access should be exposed. The gateway emits COOP/COEP/CORP, HSTS, no-referrer and a CSP allowing same-origin modules, blob workers and `wasm-unsafe-eval`, without JavaScript `unsafe-eval`.

Use service-level memory/task limits for the Node process and its validator children. [The example unit](deploy/horizon-gateway.service) sets a shared 512 MiB memory ceiling, task bound, no-new-privileges, private temporary storage and a read-only filesystem except private state. Adapt its paths and memory limit to measurements on the host, leaving headroom for the native engine, proxy and OS. It is a template, not evidence of a running deployment. A small server's viability is a measured result, not established by this gateway's tests. Native guest memory/fuel limits complement these OS limits; a wall-clock timeout alone does not bound process memory. Do not enable `MemoryDenyWriteExecute` for this unit: the native validator and Jco's WebAssembly engine require executable compiled code.

The default network bounds are 32 concurrent WebSockets, four per friend session, 2 MiB per message, bounded fragments/chunks, and 4 MiB upstream/downstream send queues. The bridge pauses reads on backpressure, limits the initial handshake to 16 KiB/ten seconds, closes idle native connections after ninety seconds, and expires sessions even while connected. [The ws API](https://github.com/websockets/ws/blob/master/doc/ws.md) documents the explicit upgrade authentication, payload bounds and pause/resume primitives used here.

[Measured native-engine memory results](deploy/memory-results.md) document the compact gameplay loader's reduction from roughly 1,082 MiB to 556 MiB idle RSS with one worker. Those local idle results are separate from deployed two-player capacity.

The provider implementation follows [Discord's OAuth2 authorization-code documentation](https://docs.discord.com/developers/topics/oauth2). The `discordFetch` dependency passed only by tests simulates Discord responses without adding an authentication bypass or configurable provider URL to production configuration.

Minecraft skin links use `minecraft:<username>` in the saved profile, independently
of the Modcraft IGN. Their latest texture descriptor is refreshed at most every
ten minutes for connected players and polled by browsers every 15 seconds. Mojang
outages keep the previous skin. Username/profile resolution and texture downloads
use fixed HTTPS hosts with redirects disabled, response/time limits and PNG
validation. The `pngjs` dependency normalizes legacy skins and strips extra image
chunks. Private `player-skins/links` metadata and SHA-256 PNGs survive releases;
include them in the same private-data backup as accounts. `minecraftFetch` is a
test-only dependency injection argument, never a production configuration URL.
