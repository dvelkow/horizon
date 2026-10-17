# Browser client and approved live mods: proposed architecture

**Status: design only. Phase 0 has not passed; phases 1–4 are NOT IMPLEMENTED.**
This document records follow-on work, not a working browser client or a deployment.
The gate remains the actual Horizon title screen rendered in Chrome, with a native
build and tests preserved. Nothing below establishes that the port is impossible.

Read [mods.md](mods.md) for the current mod contract. This design keeps native
Wasmtime and the existing WIT contract while adding browser-specific adapters.

## What exists and what is missing

| Area | Repository evidence | Consequence |
|---|---|---|
| Dedicated server | [`horizon-server` commands](../crates/horizon-server/src/main.rs#L19) and [engine launch](../crates/horizon-server/src/engine.rs#L91) | Use `horizon-server run`; it supervises `horizon-client --dedicated --nogui`. `horizon-server --dedicated` is not the CLI. |
| Network transport | [blocking connection](../crates/horizon-protocol/src/connection.rs#L108), [login/receive loop](../crates/horizon-client/src/net.rs#L246) | TCP and in-process pipes exist. A WebSocket bridge alone does not adapt the browser client. |
| Mod execution | [Wasmtime setup and instantiation](../crates/horizon-mods/src/runtime.rs#L177) | Native components have explicit imports, fuel and memory limits. Browser execution needs a separate backend. |
| Server hooks | [server-side rejection](../crates/horizon-mods/src/runtime.rs#L201), [WIT](../crates/horizon-mod-sdk/wit/horizon.wit) | Native server code mods are explicitly unsupported. World mutation, server events and mod networking are absent from the WIT. Keeping Wasmtime does not implement those hooks. |
| Reload | [dev polling](../crates/horizon-client/src/game/mods.rs#L227), [replacement](../crates/horizon-mods/src/runtime.rs#L393) | Current reload is local file modification-time polling. It drops the old instance before replacement succeeds and does not repeat `Content::read`. |
| Content registries | [startup resource layering](../crates/horizon-client/src/game/mods.rs#L50) | Blocks, items and resources load before the world. This is not a live multiplayer registry update mechanism. |

## Phase 1 — browser client and assets — NOT IMPLEMENTED

Run `horizon-prepare` natively, then publish only the prepared resource tree for the
selected version through an access-controlled asset service. Upstream treats these
prepared resources as private, excluded from the public source repository; browser
asset delivery remains an unresolved distribution design in this spike. The
browser fetches a versioned asset manifest and files, verifies
their hashes, and caches immutable assets. Put browser-owned settings and cache data
behind an explicit storage interface; use OPFS where appropriate. Replacing arbitrary
`std::fs` calls requires adapting their callers, not merely mounting a URL.

Serve the game over HTTPS with `Cross-Origin-Opener-Policy: same-origin` and
`Cross-Origin-Embedder-Policy: require-corp`. Serve matching resources from the same
origin or with explicit compatible cross-origin policies. Confirm
`crossOriginIsolated` before enabling shared-memory workers. Headers enable the
browser prerequisites; they do not turn Rust `std::thread` into working browser
threads. See [shared memory requirements](https://developer.mozilla.org/en-US/docs/Web/JavaScript/Reference/Global_Objects/SharedArrayBuffer#security_requirements)
and [OPFS](https://developer.mozilla.org/en-US/docs/Web/API/File_System_API/Origin_private_file_system).

Acceptance: the real client loads prepared assets, survives a cache reload and
reports useful startup failures. Measure native and browser frame times on the same
machine, world, camera, resolution and render distance; report the adapter, browser,
build mode and median/tail results. No FPS comparison has been established here.

## Phase 2 — browser multiplayer — NOT IMPLEMENTED

```text
Browser client -- WSS --> fixed-upstream bridge -- TCP --> dedicated Horizon engine
```

Keep packet encoding, compression and encryption in the protocol layer. Add a
browser transport using asynchronous input queues or a deliberately designed worker
adapter. Current `Connection::recv` blocks until a complete packet; the browser's
main event loop must remain responsive. Authentication requests also need an async
HTTP implementation in place of the native blocking path.

The bridge forwards bytes to one configured local engine endpoint. WebSocket message
boundaries must not become Minecraft packet boundaries: packets may be fragmented
or coalesced. Bound frame sizes, buffered bytes and concurrent connections, and
propagate closure and backpressure in both directions. Require the intended origin
and authenticated player sessions. Do not accept an upstream host or port from the
browser; that would create an open proxy. Origin checks alone are not authentication.

Acceptance: two independent browser sessions join the same dedicated world and see
each other's movement and block edits. Exercise reconnects, fragmented packets,
slow consumers and rejected sessions. Verify player identities at the server.

## Phase 3 — browser components — NOT IMPLEMENTED

The first target is the existing [`coords-hud`](../crates/horizon-mod-sdk/examples/coords-hud/src/lib.rs):
tick snapshots in, retained HUD updates and local chat out. Preserve the
[`horizon:mods@0.1.0` WIT](../crates/horizon-mod-sdk/wit/horizon.wit).

| Candidate | Fit and limitations |
|---|---|
| `jco transpile` | Preferred starting point. Generate browser JS glue and core Wasm at upload time; use explicit instantiation imports and disable default WASI shims. New approved bundles do not require rebuilding the game. [Official transpilation documentation](https://bytecodealliance.github.io/jco/transpiling.html). |
| `wasm_component_layer` with `js_wasm_runtime_layer` | Keeps hosting in Rust, but needs a compatibility spike against Horizon's components. Current component-layer source uses older component tooling and lists missing binding generation, transcoders and subtyping. The JS backend also contains unimplemented import cases. These are integration risks, not a demonstrated failure of `coords-hud`. [Project](https://github.com/DouglasDwyer/wasm_component_layer), [dependency manifest](https://raw.githubusercontent.com/DouglasDwyer/wasm_component_layer/master/Cargo.toml), [JS module backend](https://raw.githubusercontent.com/DouglasDwyer/wasm_runtime_layer/master/backends/js_wasm_runtime_layer/src/module.rs). |

For the jco route, use a pinned toolchain with instantiation mode and
`--no-wasi-shim`. Only trusted tooling generates JavaScript; reject uploaded JS and
unexpected component imports. Supply a narrow import object containing only the
approved Horizon interfaces. Do not expose DOM, fetch, storage, credentials or
arbitrary callbacks to guest Wasm. Maintain native Wasmtime in target-specific
dependencies; disabling it without a browser replacement does not complete mods.

Browser WebAssembly has no Wasmtime fuel or store limiter API. Execute each mod in
a separate dedicated Worker, send bounded snapshots and outputs, and terminate
unresponsive workers from an external watchdog. Validate or instrument every core
module to bound memories and tables, including aggregate limits across modules.
An elapsed-time watchdog is not equivalent to deterministic instruction fuel;
equivalent fuel requires additional trusted instrumentation. See the
[WebAssembly JavaScript API](https://webassembly.github.io/spec/js-api/) and
[Worker termination](https://developer.mozilla.org/en-US/docs/Web/API/Worker/terminate).

Keep the existing HUD/chat caps at the host boundary and bound log/output rates and
message queues. Native approval-time `init` checks do not establish that later
browser hooks are safe. Test infinite loops, allocation attempts, traps, forbidden
imports and oversized outputs next to a healthy mod.

Acceptance: the actual `coords-hud` component renders current world coordinates,
supports `/where` and `/coords`, reloads without a game rebuild, and a failing mod
does not freeze the game. Native sandbox tests continue to pass.

## Phase 4 — upload, approval and atomic reload — NOT IMPLEMENTED

1. Authenticate the uploader and stream a bounded `.hmod` into private quarantine.
   Reject duplicate paths, unexpected entries and excessive manifest/entry counts
   before allocation or extraction. Apply compressed and expanded size limits.
2. Reuse [`Manifest::parse` and `check_imports`](../crates/horizon-mods/src/manifest.rs#L119),
   [`component_imports`](../crates/horizon-mods/src/manifest.rs#L252) and
   [`Content::read`](../crates/horizon-mods/src/content.rs#L146). Validate the complete
   component, instantiate it and run `init` under native fuel and memory limits.
   Execute parsing, compilation and transpilation in an isolated process with
   separate CPU, memory and wall-clock limits: guest fuel does not cover those steps.
3. Store immutable candidates by SHA-256. Present the exact candidate's identity,
   capabilities, validation result and changes for owner approval. Approval binds
   that hash; a later upload cannot replace the approved bytes.
4. Publish the approved immutable bundle and its hashes before atomically changing
   the index revision. Serialize concurrent approvals. Notify clients over an
   authenticated WebSocket only after the new index is available.
5. Clients fetch the revision, verify all artifact hashes and stage a candidate
   Worker. Only replace the running instance after initialization succeeds. Preserve
   the previous instance on failure; discard stale responses and reset commands/HUD
   atomically. On reconnect, reconcile against the current index instead of relying
   on having received every notification.

The existing [32 MiB code cap](../crates/horizon-mods/src/manifest.rs#L16) and
[512 MiB content cap](../crates/horizon-mods/src/content.rs#L17) are format limits,
not evidence that such uploads fit a small host. Start with stricter operational
limits and measure. The existing readers ignore unrelated ZIP entries and do not
provide the full strict upload policy described above.

Initially allow live replacement only for supported client code hooks. Content
changes affecting registries require a coordinated world restart/reconnect until
server-owned registry synchronization and migration exist. Native server gameplay
hooks require their own versioned WIT and host implementation; do not mark
`side = "server"` or `side = "both"` as fully supported through portal approval.

Acceptance: an unapproved upload never runs, approval releases exactly the reviewed
hash, connected clients receive the update, reconnecting clients catch up, and
invalid candidates or races preserve the previous working version.

## Deployment boundary for the existing small DigitalOcean host

No host has been provisioned or changed by this design. Inspect existing capacity
and services before choosing limits; no claim is made that the requested plan can
hold this workload. Build/preparation and expensive validation can run off-host,
while the host serves immutable assets and runs the engine and bridge. Measure RAM,
world tick time, player count and upload-validation contention before enabling more.

The DigitalOcean API credential belongs only to a private provisioning process.
The game, bridge, mod validator and browser have no reason to receive it. Keep it
out of source control, build arguments, browser environment variables, asset trees,
source maps, URLs and logs. If local persistence is necessary, use a private file
outside every served directory with restrictive permissions; inject it only into
the provisioning operation and avoid printing it. Do not copy it onto the game host
merely to run the server.

Run services as distinct unprivileged identities. Keep engine TCP and control
endpoints private; expose only the HTTPS game gateway to players. Put approval and
deployment control behind a separately authenticated administrative boundary, such
as a private interface reached over SSH. Browser sessions get only scoped gameplay
credentials. Validator processes receive neither provisioning credentials nor
administrative sessions. Serve only explicit asset/bundle directories, never a
repository checkout, home directory or generic upload directory.
