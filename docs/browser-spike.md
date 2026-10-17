# Phase 0: browser platform and title bootstrap

This branch ports the existing Rust client to `wasm32-unknown-unknown` and starts its original title through WebGPU. Native runtime paths remain selected outside wasm32. The branch is limited to the title/platform spike; later phases add durable storage, networking and active mods.

| Baseline blocker | Phase 0 change and remaining boundary |
|---|---|
| wgpu Vulkan/Metal only | Select WebGPU by target; initialize the adapter/device asynchronously. Native Vulkan/Metal remain unchanged. |
| winit web lifecycle | Attach the real renderer to the browser canvas and use the browser event loop. |
| `std::thread` | Shared-memory module Workers through vendored `wasm_thread`, with wasm-bindgen-compatible TLS startup and no JavaScript eval. |
| Blocking Window synchronization | Browser mutex/RwLock acquisition avoids unsupported Window parking; Workers retain blocking synchronization. Scoped preparation runs serially. |
| `std::fs` and path metadata | A shared in-memory virtual filesystem and `PathExt` provide the same resource/path interface. Minimal bounded, verified fetch preloads native-prepared resources; OPFS durability is a later phase. |
| `Instant::now` | `web-time` supplies browser clocks throughout supporting crates; Chrono uses its browser backend. |
| `ureq` | Target-gate native HTTP and add asynchronous browser fetch. Native TCP is unavailable in browser gameplay until the separate networking phase. |
| `pollster` | Renderer startup awaits browser promises instead of blocking the Window. |
| `arboard` | Asynchronous browser clipboard handling, with stale paste protection. |
| `cpal`, HID and input | Browser audio/gamepad adapters and gesture activation; retain native audio/HID implementations on their platforms. |
| Texture-array limits | Split sprite banks at the adapter's reported layer limit. |
| WGSL validation | Move HUD sampling before fragment-dependent branches and surface shader/device errors visibly. |
| Wasmtime/Cranelift | Keep the native runtime target-gated; provide the browser API facade without installing an active JavaScript mod host in this phase. |

Build prerequisites are nightly-2026-10-17 with `rust-src` and the wasm32 target, plus wasm-bindgen CLI 0.2.129. `web/build.sh` pins that compiler by default, enforces the binding-tool version, uses Cargo's lockfile, and supplies shared-memory/atomics/TLS flags. A plain stable wasm build is not the threaded release procedure.

Run native `horizon-prepare` for the pinned resource version, then `python3 web/prepare-assets.py --help` for deterministic bundling. `python3 tools/serve_web.py` serves the generated `.local/web` directory on loopback with COOP/COEP. A secure context, cross-origin isolation and browser WebGPU are required. This phase's launcher opens the original title directly.

Isolated branch validation: native client/server `cargo check` and the nightly atomics/build-std browser check passed. These are compile checks; exact-tree rendered-title and FPS verification are separate evidence. The complete integration branch records its Chrome rendering observations rather than attributing later multiplayer or mod behavior to this branch.

Implementation references: [platform adapters](../crates/horizon-platform/src/lib.rs), [worker runtime](../vendor/wasm_thread/src/wasm32/mod.rs), [resource bootstrap](../crates/horizon-client/src/browser.rs), [renderer](../crates/horizon-client/src/gfx.rs), and [game lifecycle](../crates/horizon-client/src/game.rs).
