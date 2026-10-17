# Playing and updating the hosted browser world

Friends open the HTTPS play URL, enter their chosen name and the shared invite,
and join the native Horizon world. Use a current desktop Chrome or Edge with
WebGPU enabled. The first visit downloads the game and prepared resources;
subsequent visits reuse content whose SHA-256 still matches. WASM Workers require
COOP `same-origin` and COEP `require-corp`; the gateway supplies both.

The operator's private access file contains the play URL, friend invite and a
separate owner approval key. Neither key belongs in source control. Cloud API
credentials are used only by the operator's provisioning process, outside the
repository, host application and browser bundles. They are not application
configuration and must never be uploaded to a served directory.

## Make a mod with an agent

While playing in the browser, press **Esc** to open the classic Minecraft pause
menu. **Options** opens the game settings. The gold **DEV MODE** button below
Disconnect opens the Mod workshop in a separate tab, keeping your world tab
connected. The workshop uses your existing friend session; publishing a mod
still requires the owner's separate approval. **Back to Game** resumes play.
If Chrome releases the mouse without sending Escape to the game, the pause
menu still opens. Native menu preferences remain available as before.

Read [the existing SDK and WIT](mods.md) first. An agent can create/build a mod
using `cargo horizon-mod new` and `cargo horizon-mod build`, then hand its `.hmod`
to the Mod workshop linked from the play page. Native `cargo horizon-mod run`
still supports the local authoring loop. Agents do not need the cloud token or
owner approval key to write/build mods.

1. Sign in with the friend invite and upload the `.hmod` in Mod workshop.
2. The host checks bounded archive paths/sizes, manifest, actual component imports
   against declared capabilities, content fences, and native sandbox initialization
   within fuel. Client components are transpiled and their memories/tables capped.
3. The owner opens the same workshop and signs in with the separate approval key.
   Review the manifest, capabilities, validation results, uploader and exact hash.
4. Approve that hash. The immutable index receives a new revision; connected
   browsers receive a WebSocket notification, verify the downloaded bytes and
   initialize a replacement before swapping. The game client itself is not rebuilt.
5. Revoke an active version to unload it. A failed replacement keeps the last
   working mod. A guest that traps or exceeds its time/memory budget stops without
   restarting the game or other guests.

A shared invite establishes membership, not unique ownership of every player
name. Reserve operator names in `protectedNames`; the gateway requires an owner
session before issuing one. The production native TCP port is loopback-only.
There is no public native administration endpoint.

## Runtime choice and limits

The browser uses Jco 1.39.0 to transpile components on the server into JavaScript
bindings and core WebAssembly, then runs them with the browser's own engine in
individual dedicated Workers. This reuses the existing component/WIT packaging
and avoids compiling a native Cranelift JIT inside WASM. A JS backend through
`wasm_component_layer` would add another embedded engine/API adaptation layer;
Jco directly exposes the typed bindings already needed by the browser host.
Native server mods continue to use Wasmtime and fuel.

Guest imports are supplied explicitly from the declared WIT capabilities: no
WASI, filesystem, arbitrary sockets or process access. Generated code is trusted
transpiler output; uploaded JavaScript is not accepted. Browser output is bounded,
commands and lifecycle callbacks are serialized, and queued snapshots coalesce.
Each browser mod has at most 64 MiB of core memory and 10,000 table entries;
at most eight client/both mods may be active. A Worker deadline terminates runaway
execution. This is wall-clock isolation, not the native instruction-fuel model.

API 0.1 supports game snapshots, HUD, chat and registered commands. New block or
item registries and baked resource/content changes require a coordinated release;
the live portal rejects their activation explicitly. This deployment does not
claim arbitrary Java/Fabric mods or new game-engine hooks can be hot-loaded.

Server/both mods are mirrored only after approval into a private directory.
The native engine polls content hashes, initializes before replacing, and retains
its previous working instance on failure. Server clock/chat hooks run in the
native world. The server has no local player snapshot. Existing vanilla command
names retain precedence.

## Resources and persistence

Run `horizon-prepare` natively for the pinned Minecraft version. Keep the prepared
files out of Git. `web/prepare-assets.py` produces deterministic ZIP bundles and
per-file/per-bundle SHA-256 receipts. The client verifies cached bytes before
preloading the immutable `/resources` virtual filesystem.

Prepare sound files before packaging the browser resources:

```sh
python3 web/prepare-sounds.py --resources .local/horizon-client/26.3 --download
python3 web/prepare-assets.py --resources .local/horizon-client/26.3
```

The sound preparation checks the pinned official asset index, verifies every
download, and emits a small SHA-256 sound catalog. The 4,961 Ogg files (377 MB)
stay outside the 23.7 MB startup resource bundles. The browser downloads sounds
as needed with its authenticated session, checks their hashes, and keeps a
bounded 64 MiB persistent cache. Browser registration recognizes verified catalog
entries before their files arrive. Long music retains at most 64 MiB of mono PCM;
decoding the longest track can temporarily require about 305 MiB in Web Audio.

Mutable settings and saves are checkpointed to OPFS. An atomic catalog references
immutable blobs, so an interrupted batch preserves the preceding committed
checkpoint. Cross-tab Web Locks protect a reader's entire restore operation;
exclusive commits collect obsolete/orphan blobs after they are safe to remove.
A 512 MiB live-save limit bounds persistence. Storage failures remain visible.
A completed virtual-file write is not yet a durable OPFS commit; allow the
checkpoint to finish before closing the page. Hosted world saves live on the
server, independently of browser storage.

## Deployment and operation

The verified host is an Amsterdam `s-1vcpu-1gb` Ubuntu droplet. Install prerequisites
with `packaging/web/install-host.sh`. Build native binaries on the development
machine, then install these explicit paths:

| Host path | Contents |
|---|---|
| `/opt/horizon/bin` | `horizon-server`, `horizon-client`, `horizon-mod-validator` |
| `/opt/horizon/resources` | Native prepared resources |
| `/opt/horizon/web` | Only generated browser release files, bundles and mod host |
| `/opt/horizon/gateway` | `web/server` and locked production dependencies |
| `/opt/horizon/mod-tools` | Jco and locked dependencies, never served |
| `/var/lib/horizon/game` | World, supervisor configuration and backups |
| `/var/lib/horizon-portal` | Private candidates, approval index and server mirror |
| `/etc/horizon/gateway.json` | Mode-0600 gateway configuration with scrypt hashes |

Use the systemd units and small-host configuration in `packaging/web`. Services
run as the unprivileged `horizon` user with restricted writable directories.
Caddy terminates HTTPS and proxies the same origin to `127.0.0.1:8080`. Only
HTTPS/HTTP and operator-restricted SSH are exposed by the cloud firewall; native
25565, gateway 8080 and administration 8130 stay private. The host has 2 GiB swap
for transient startup/validation pressure, one generation Worker and view distance four.
World backups run every six hours, retaining four automatic copies on this host.
They are not off-host disaster recovery.

Inspect `systemctl status horizon-game horizon-gateway caddy` and their journals.
`/healthz` proves gateway availability only; inspect the native supervisor status
and perform a real login to establish game readiness. Keep public files and
private state separate during every update. Stage a complete browser release
before switching it into service so its JavaScript, WASM and Worker imports match.

### Repeatable browser updates

Configure an operator SSH alias that connects as root; the updater uses SSH only,
not a cloud API token. Build and prepare resources locally, then stage a named
release. Use a fresh release ID for each build:

```sh
./web/build.sh
bash packaging/web/update-web.sh stage horizon-host 20261017-01 .local/web
# Review the staged release, then activate during a reconnect window:
bash packaging/web/update-web.sh activate horizon-host 20261017-01
```

The updater allowlists generated browser files, verifies transferred SHA-256
checksums, and stages under `/opt/horizon/releases/<release>`. It explicitly uses
`rsync --chmod=D755,F644`: temporary asset files created with mode 0600 otherwise
remain unreadable to the service account and caused failed asset responses during
deployment. Private configuration, world state, cloud credentials, dependencies
and diagnostic pages are not uploaded by this script.

Staging does not restart a service. Activation switches `/opt/horizon/web` to the
complete release and restarts only `horizon-gateway`. The gateway resolves served
roots on startup and stores sessions in memory, so players must sign in again.
The first activation migrates an existing physical `web` directory while the
gateway is stopped; later symlink switches are atomic. A failed service restart
restores the previous link. A healthy process is not sufficient verification:
check the HTTPS `/healthz`, login, resource loading and an actual world join.

Old releases and their hashed resources are retained; unchanged resources use
hardlinks to avoid another full copy. Do not use `--delete` or `--inplace` on these
trees. Keep old hashes while tabs or caches may reference them, and prune only
during planned maintenance after checking active sessions and rollback needs.
To roll back a release created by the updater, activate its previous release ID.
The first migrated tree is printed as a `legacy-*` path; restore that symlink
manually and restart the gateway if rolling back the initial migration. Run one
deployment at a time. An interrupted `.staging` directory remains inactive; inspect
and remove that exact staging directory before retrying the same ID.

Keep `/etc/horizon/gateway.json` mode 0600, owned by `horizon`, outside the served
release. Its production roots are `/opt/horizon/web`, `/opt/horizon/web/assets`
and `/opt/horizon/web/mods-runtime`. Gateway code/Jco updates are installed in
their separate non-served directories from locked dependencies, then require an
explicit `systemctl restart horizon-gateway`. Neither this restart nor a browser
release needs to restart the native world.

For native engine or prepared-resource changes, schedule a world reconnect window
and use a separate explicit operation: stage new binaries/resources outside served
directories, `systemctl stop horizon-game`, confirm the graceful stop completed,
install the verified binaries with mode 0755 and resources with directory/file
modes 0755/0644, then `systemctl start horizon-game`. Preserve the previous
binaries/resources and a world backup for rollback. Do not overwrite a live
binary in place, and do not restart the game for an ordinary approved API 0.1 mod
update. Inspect supervisor status and a real join after restarting.

## Verification and performance

On 2026-10-17, two independent browser contexts joined the same world through the
public HTTPS host. Movement changed player coordinates from
`96.5 / 145 / -31.5` to `96.5 / 142 / -36.6`; chat reached the server. The actual
workshop UI uploaded and approved a 24,709-byte `coords_hud` 0.1.1 package. Both
running clients changed from version 0.1.0's `XYZ` panel to `Live XYZ`, without
reloading or rebuilding the client. The approved SHA-256 was
`d0722e4225457c9db91cf037516dea9b459ce2eaef40c34891d0793846df0b84`.

The final public release also passed an actual in-game sound check after trusted
input: three authenticated sound downloads decoded successfully, the Rust mixer
produced a peak of 0.5698 and the browser audio output measured 0.5649 at 44.1 kHz.
The player remained in the world with `coords_hud` 0.1.1 running. The recorded
sample is `.local/browser-spike/production-final-audio-state.txt`.

The native workspace test run passed 1,444 tests across 94 suites with zero
failures and 17 ignored tests. The final sound-registry correction additionally
passed all three registry tests, including verified lazy resources and native
disk-only behavior. Strict workspace Clippy, formatting, browser compilation and
all four sound-preparation regression tests passed.

That two-player host sample reported 400 chunks, 19.9879 TPS, mean tick time
3.638 ms, p95 6.117 ms and p99 33.562 ms. The game service used about 345 MiB RAM
plus 285 MiB swap; the gateway used 57 MiB RAM plus 11 MiB swap. These are measured
samples, not an eight-player capacity guarantee. Local verification artifacts
include `two-browser-production-status.json`, `production-live-before/after.txt`
and the production screenshots under `.local/browser-spike/`.

A separate controlled title-panorama comparison used the same NVIDIA RTX 4060 Ti,
1280×720 rendering, view distance six, foreground windows and recent mouse input.
Native and browser runs were sequential with only one active game renderer.

| Build | Rendered frames / measured seconds | Average FPS | Frame p50 / p95 |
|---|---:|---:|---:|
| Native Vulkan | 732 / 20.0336 | 36.5385 | 27.1123 / 29.9529 ms |
| Chrome WebGPU | 561 / 19.3232 | 29.0324 | Not instrumented |

Browser/native FPS was 0.79457 in this test. Chrome reported a non-fallback NVIDIA
Lovelace WebGPU adapter, but its Xvfb/ANGLE presentation used Mesa llvmpipe.
Browser and native builds also used optimization levels two and three,
respectively. This is a local title-screen comparison, not ordinary desktop or
deployed-world FPS. Browser frame percentiles cannot be inferred from the
once-per-second counter. Exact samples and controls are in
`.local/browser-spike/fps-comparison-controlled.json` and the corresponding
`*-title-controlled-fps.json` files.

To repeat, keep the other game closed, use a fresh foreground title screen at
1280×720, warm it up and move the mouse regularly to prevent AFK throttling.
For native, start with a private `--control-socket` and use
`horizon-ctl --socket <path> bench 20`. For browser, sample
`window.horizonPerformance` before and after about twenty seconds and calculate
`1000 * (last.frames - first.frames) / (last.lastTimestamp - first.lastTimestamp)`.
Record adapter, resolution, scene, focus, build profile and presentation backend
with each result; do not compare samples taken while another game is running.

For service configuration, API endpoints and validation tests see
[the gateway guide](../web/server/README.md). Measured memory and its regression
checks are in [the hosting measurement](../web/server/deploy/memory-results.md).
