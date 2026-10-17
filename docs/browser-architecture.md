# Hosted Horizon browser architecture

The initial feasibility inventory has been implemented; see
[the Phase 0 result](browser-spike.md) and [the operator/author guide](browser.md).
The deployed game retains native simulation and the existing Rust client.

```text
Browser
  Rust Horizon + winit + WebGPU
    shared WASM memory <-> streaming Workers
    immutable resource VFS <- verified fetch/Cache Storage
    settings/save VFS <-> atomic OPFS checkpoints
    protocol codecs <-> authenticated /game WebSocket
    mod facade <-> isolated component Workers
              |
        same-origin HTTPS (Caddy)
              |
  private gateway on loopback:8080
    invite/owner sessions, exact Origin and CSRF checks
    /game -> fixed loopback TCP:25565
    prepared resource bundles
    uploaded .hmod -> native validator -> pending review
    owner approval -> immutable SHA-256 index -> /mods/events
                            |
             private approved server-mod mirror
                            |
  native Horizon dedicated engine + Wasmtime
    world simulation and saves, supervisor and backups
```

## Implementation boundaries

- `horizon-platform` supplies target-specific filesystem, clocks, threads and
  synchronization. Native builds keep standard-library behavior. Browser Window
  code must not park waiting for a promise or an OS thread.
- The browser entrypoint initializes resources asynchronously, owns the game for
  the web event-loop lifetime, and installs the renderer after GPU promises resolve.
- `horizon-protocol::connection::browser_transport` owns bounded packet framing,
  compression and cryptography independently of TCP. The browser session handles
  WebSocket login/configuration/play events without blocking reads. Sending copies
  out of shared WASM memory because WebSocket BufferSource forbids shared views.
- The gateway chooses the destination. A client cannot turn it into an arbitrary
  TCP proxy. Its first offline login identity must match its signed-in session.
- Native validation reuses manifest, import/capability and content checks, then
  performs fuel-limited initialization. Jco emits browser artifacts; emitted core
  memories/tables are inspected and bounded before publication. The browser
  verifies their hashes and exposes only the declared host interfaces.
- Browser guests run in individual Workers with deadlines and bounded messages.
  Native guests retain Wasmtime fuel and memory limits. These are different
  execution-budget mechanisms with the same capability contract.
- Owner approval is the publication boundary. The manifest's requested
  capabilities are visible before approval; a hash-specific decision atomically
  changes the active index. Updates use the existing game mod reload/output path.
- Registry/content activation is a coordinated release, not an executable HUD
  reload. The portal refuses unsupported live activation instead of silently
  mixing incompatible registries.

## Secrets and persistence

Only the operator's local provisioning process needs the cloud credential.
The host's gateway configuration contains scrypt invite/owner hashes, not cloud
credentials. Service state and candidate files live outside the served roots.
The public game protocol/admin ports are closed; authenticated HTTPS is the sole
player entry. Native administration remains loopback/private.

Server saves and browser OPFS checkpoints are separate. Server world persistence
uses Horizon's existing save path and automatic supervisor backups. Browser
checkpoints protect local settings/saves across page reloads, with cross-tab
restore locks and atomic catalogs. Neither a frame render nor an in-memory VFS
write is a claim that an OPFS commit has completed.

See `web/server/README.md` for API contracts, bounded queues, validation subprocess
limits and deployment assumptions. The live workflow intentionally stays within
Horizon API0.1; it does not add Java mod execution or arbitrary engine mutation.
