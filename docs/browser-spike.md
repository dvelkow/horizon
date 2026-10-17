# Phase 0: browser feasibility

The genuine Horizon title screen renders in Chrome with WebGPU. The browser
client is the Rust game, built for `wasm32-unknown-unknown`; it is not a
JavaScript recreation of its title screen. Native rendering remains Vulkan/Metal.
The initial compile blockers at upstream revision
`1af78463ebf7148d71e8a8472564a80cc392544e` have been addressed as follows.

| Blocker | Implemented solution |
|---|---|
| Native graphics backend | Target-specific WebGPU, async adapter/device initialization, winit canvas lifecycle. |
| Thread creation | Shared WASM memory and module Workers; vendored `wasm_thread` adapted to current wasm-bindgen initialization without JavaScript eval. Native threads are unchanged. |
| Blocking the browser Window | Window mutex/RwLock contention uses nonparking acquisition; Workers use normal blocking synchronization. Scoped preparation is serial. Saves and screenshot readback complete asynchronously. |
| Filesystem | `horizon-platform` provides a shared virtual filesystem, preloaded resources, and mutable settings/save files. `PathExt` routes path queries through the same filesystem. Native paths still use `std::fs`. |
| Time | `web-time`, including supporting crates; Chrono's browser clock is enabled. |
| Native HTTP and sockets | Browser fetch and asynchronous WebSocket transport. Direct TCP, native discovery and Microsoft account login remain native features; hosted browsers use the gateway invite/session. |
| Clipboard, HID, audio | Asynchronous browser clipboard, browser audio/gamepad paths, native-only HID output. |
| Browser texture limits | Sprite banks split at the adapter's actual array-layer limit, with twelve banks available for 256-layer adapters. |
| WGSL validation | HUD sampling occurs before fragment-dependent branches, satisfying Chrome's derivative-uniformity rule. Shader/device errors produce a persistent visible error. |
| Wasmtime/Cranelift | Native runtime is target-gated. Browser mod interfaces connect to the separate Worker runtime described in `browser.md`. |
| Prepared resources | Pinned 26.3 resources were prepared natively using Java 25, then packaged into authenticated, hashed browser bundles. |

The implementation requires cross-origin isolation, WebGPU, SharedArrayBuffer,
and a desktop Chrome/Edge browser. Native shader packs and the native integrated
server are explicitly unavailable in the browser; the hosted multiplayer world
is the supported entry path. This does not extend Horizon's underlying gameplay
or mod WIT beyond its existing capabilities.

## Build and reproduce

Native builds use the existing Rust toolchain and normal Cargo commands. The
browser build additionally needs nightly Rust with `rust-src`, the WASM target,
and wasm-bindgen CLI 0.2.129. The verified nightly was 2026-10-17.

```sh
rustup toolchain install nightly-2026-10-17 --component rust-src --target wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.129 --locked
python3 web/prepare-assets.py --resources .local/horizon-client/26.3
./web/build.sh
python3 tools/serve_web.py
```

The local preview at `http://127.0.0.1:8080` opens the actual title screen without
a gateway. The production launcher authenticates and joins the hosted world.
Shared WASM requires rebuilding the standard library with atomics, imported
shared memory, and TLS exports; `web/build.sh` includes those flags. A plain
stable `cargo build --target wasm32-unknown-unknown` is not the threaded release
build procedure.

## Verification notes

Chrome's Vulkan headless compositor on this workstation produced blank
screenshots even when a WebGPU clear readback returned the correct pixels.
Verification therefore used headed Chrome under Xvfb with WebGPU enabled and
ANGLE OpenGL. The real title screenshot is recorded in the local verification
artifacts, alongside a separate clear/readback check proving that compositor
issue. Rendering was also checked for actual shader validation errors: a logged
"first frame" alone is insufficient evidence of a displayed title.

Native tests, multiplayer, deployment and performance results are recorded in
`browser.md` and `web/server/deploy/memory-results.md`. The old initial no-go
report described the unported baseline and is superseded by this implementation.
