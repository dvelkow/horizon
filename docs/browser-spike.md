# Phase 0: browser feasibility spike

**Status: BLOCKED / NO-GO for proceeding to deployment. The title-screen gate
has not passed.** This is a partial spike, not a browser release. There is no
playable browser URL, two-browser world, browser mod runtime, or approval portal.
No DigitalOcean resources were accessed or changed.

The inspected upstream revision is `1af78463ebf7148d71e8a8472564a80cc392544e`
(2026-10-17). The only client change in this spike selects `wgpu/webgpu` for
`wasm32`, retaining Vulkan/Metal and the existing common features for native
builds. The lockfile records the additional feature dependencies. All remaining
platform work below is **unimplemented**, not hidden behind a substitute title
screen or disabled mod sandbox.

## Results

| Check | Result |
|---|---|
| Original `cargo +stable build --locked -p horizon-client --bin horizon-client --target wasm32-unknown-unknown` | Fails in `getrandom 0.2.17`: browser randomness backend is not enabled. |
| Same build with the WebGPU selection | Same failure; enabling a graphics backend does not remove native dependencies. |
| Separate `hidapi` WASM check | Build script panics: `Unsupported target os for hidapi-rs`. |
| Separate `socket2` WASM check | `Socket2 doesn't support the compile target`, followed by missing platform implementation errors. |
| Separate `arboard` WASM check | Missing platform `Clipboard`, `Clear`, `Get`, and `Set` types. |
| Temporary compile-only dependency probe enabling both getrandom JS backends | Gets beyond the initial randomness failure, then native `ring` C compilation requires absent `clang`. This is a fixable local toolchain issue, not evidence that TLS or the port is impossible. The probe is not part of the game. |
| Native `cargo +stable build --locked -p horizon-client --bin horizon-client` | Pass. |
| Native `cargo +stable test --workspace` | **Pass: 1,429 passed, 0 failed, 16 ignored**, across 91 test-result summaries. Includes actual SDK component and native mod sandbox tests. |
| `cargo +stable fmt --all -- --check` | Pass. |
| `cargo +stable clippy --workspace --all-targets --locked -- -D warnings` | Pass. |
| `python3 tools/check_public_tree.py` and `--history` | Pass. |
| `python3 -m unittest discover -s tests -v` | 236 tests run; 1 environment error in unchanged `test_supervise`: system Git does not support the required modern `merge-tree` invocation. This is separate from the passing Rust suite. |
| `horizon-prepare --prepare` with the installed Java 25 JDK | Fails: missing `versions/26.3/26.3.json`. Local Minecraft versions are 26.1.2 and 1.21.4; no matching prepared resources or pinned server JAR were found. |
| Chrome title screen | **Not reached.** No client WASM artifact was produced. |
| Native versus browser FPS | **Not measured.** Requires the runnable browser client and matching prepared scene resources. |

Tests were run with Rust/Cargo stable 1.99.1 and the `wasm32-unknown-unknown`
target installed, on Linux x86-64. Resource-dependent upstream tests can return
early when prepared data is absent; passing this suite is not a rendering or
gameplay verification. The native tests are development builds, not an FPS
baseline. Compiler failures stop before typechecking the complete client, so
this inventory cannot promise that every later compiler error has been found.

## Blocker inventory

Source locations refer to the inspected revision. Literal source scans found
25 client source files containing `std::thread`, 62 containing `std::fs`, and
60 containing `Instant::now`. The inventory tool's best-effort import/alias scan
finds 29, 111 and 60 respectively. These include tests, comments and inactive
branches: they are review locations, not a count of required edits.

| Area | Evidence | Work needed |
|---|---|---|
| GPU backend | [client manifest](../crates/horizon-client/Cargo.toml) | WebGPU target selection is added. Actual WebGPU rendering remains unverified. |
| Event loop | [`game.rs`](../crates/horizon-client/src/game.rs#L10503), [`main.rs`](../crates/horizon-client/src/main.rs) | Add a browser entrypoint and canvas, use winit's web event-loop lifecycle, and persist owned game resources beyond startup. The current `Game<'a>` borrows startup-local resources. |
| Blocking GPU initialization | [`game.rs:7754`](../crates/horizon-client/src/game.rs#L7754), [shader-pack scopes](../crates/horizon-client/src/gfx/pack_stages.rs#L163) | Replace `pollster::block_on` paths reached in the browser with async initialization/completion events. Browser promises need the event loop to run. |
| Resource reads and writes | [`Assets::load`](../crates/horizon-client/src/assets.rs#L96), [eager startup](../crates/horizon-client/src/game.rs#L10285) | Introduce a resource abstraction with fetch/preloaded bytes and persistence adapters for settings/cache/saves. Native `std::fs` does not become OPFS automatically. |
| Threaded preparation and streaming | [HUD atlas](../crates/horizon-client/src/hud.rs#L233), [streaming](../crates/horizon-client/src/streaming.rs#L343) | Atlas preparation uses scoped threads; streaming starts workers even at title. Start with serial preparation for the spike, then port workers and queues explicitly. COOP/COEP alone cannot implement `std::thread`. |
| Clocks | [title clock](../crates/horizon-client/src/title.rs#L233), [front-end timing](../crates/horizon-client/src/front.rs#L16) | Introduce a compatible `web-time` clock across browser-reachable paths, including supporting crates. |
| Texture-array limits | [2,048-layer split](../crates/horizon-client/src/gfx.rs#L377), [limit check](../crates/horizon-client/src/gfx.rs#L604) | Split using actual adapter limits. WebGPU guarantees only 256 layers; four such arrays may not hold the full sprite set. Validate capacity and pipeline bindings in a real browser. |
| Clipboard | [system clipboard](../crates/horizon-client/src/clipboard.rs#L130) | Gate `arboard` to native; implement asynchronous browser clipboard handling and in-game fallback. |
| Audio | [audio backend](../crates/horizon-client/src/audio.rs), [client manifest](../crates/horizon-client/Cargo.toml) | Enable/evaluate CPAL's browser backend and user-gesture audio activation. Adapt audio worker assumptions and resource loading. CPAL itself should not be labelled inherently native-only. |
| Controllers | [DualSense manifest](../crates/horizon-dualsense/Cargo.toml), [gamepad](../crates/horizon-client/src/gamepad.rs) | Keep HID output native-only. Evaluate gilrs' existing web backend and browser gamepad input separately. |
| Randomness | [friends manifest](../crates/horizon-friends/Cargo.toml), [protocol manifest](../crates/horizon-protocol/Cargo.toml) | Enable appropriate JS randomness for every reached getrandom version after removing native-only dependency paths. |
| HTTP/authentication | [`auth.rs`](../crates/horizon-client/src/auth.rs), [skins](../crates/horizon-client/src/skins.rs) | Replace blocking ureq/native TLS with fetch and adapt callers. Account for service CORS and authentication flow. |
| Sockets and protocol | [friends sockets](../crates/horizon-friends/Cargo.toml), [connection](../crates/horizon-protocol/src/connection.rs#L108), [client receive loop](../crates/horizon-client/src/net.rs#L246) | Gate native networking and reuse packet codecs behind an async/worker browser transport. A WebSocket-to-TCP bridge alone does not change the client transport. |
| Control, logging and process APIs | [control](../crates/horizon-client/src/control.rs), [startup](../crates/horizon-client/src/main.rs), [logging](../crates/horizon-client/src/logging.rs) | Keep local control sockets, file logs/crash reports, OS paths and process exit on native; provide browser lifecycle and diagnostics. |
| Component runtime | [mod manifest](../crates/horizon-mods/Cargo.toml), [runtime](../crates/horizon-mods/src/runtime.rs) | Separate native Wasmtime/Cranelift from browser compilation. Browser components need the browser engine plus host bindings; a stub runtime is not mod support. |
| Prepared resources | [`research-lock.json`](../research-lock.json), [preparer](../crates/horizon-prepare/src/main.rs) | Supply the pinned 26.3 installation and server JAR, use Java 25, and run preparation. The preparer checks versions/hashes; older installed assets are not an interchangeable substitute. |

Native features such as timestamp queries and indirect-first-instance are already
conditional. No unavoidable WGSL incompatibility was established by the code
review. This is a **blocked implementation and verification**, not a claim that
Horizon can never run in a browser.

## Smallest genuine route to the Phase 0 gate

1. Separate title startup from world startup and native services. Reuse the
   existing [`Title::draw`](../crates/horizon-client/src/title.rs#L775),
   [Horizon title layout](../crates/horizon-client/src/front.rs#L110),
   [HUD renderer](../crates/horizon-client/src/hud.rs#L815), and
   [panorama](../crates/horizon-client/src/gfx/panorama.rs#L69).
2. Load the required prepared assets through a browser resource interface;
   build the title atlas without native threads. Make clocks and clipboard
   browser-compatible, and remove unrelated native dependencies from this path.
3. Initialize WebGPU asynchronously into the actual winit canvas. Handle resize,
   device failures and unsupported browser features with visible errors.
4. Run the genuine title in Chrome, capture the frame and console results, then
   repeat native build/tests. Only then call the spike a pass and begin Phase 1.

The subsequent transport, runtime and approval work is described in
[browser-architecture.md](browser-architecture.md). In particular, executable
server hooks are **not already implemented** and local mtime hot reload is not a
remote update mechanism. Registry/content changes need more than replacing a
running HUD component.

## Reproduce

Use Python 3.11+ and the current Rust stable toolchain. Native tests also need
ALSA and udev development headers on Linux. The preparer needs Java 25.

```sh
rustup toolchain install stable --profile minimal --component rustfmt --target wasm32-unknown-unknown
cargo +stable build --locked -p horizon-client --bin horizon-client --target wasm32-unknown-unknown
cargo +stable test --workspace --locked
cargo +stable fmt --all -- --check

# Read-only source/dependency/resource inventory; JSON on stdout.
python3 tools/browser_spike.py

# Actual build, timestamped log and JSON, preserving Cargo's failing exit code.
# The tool does not install Rust or claim browser support on compilation success.
RUSTUP_TOOLCHAIN=stable python3 tools/browser_spike.py --build

# Independent checks of dependencies whose first errors the full build masks.
cargo +stable check --locked -p hidapi --target wasm32-unknown-unknown
cargo +stable check --locked -p socket2 --target wasm32-unknown-unknown
cargo +stable check --locked -p arboard --target wasm32-unknown-unknown
```

The source inventory includes file and line lists. Logs default to the ignored
`.local/browser-spike/` directory; game resources and credentials must remain
outside committed/public artifacts.

## Platform references

- [Rust target restrictions](https://doc.rust-lang.org/rustc/platform-support/wasm32-unknown-unknown.html): filesystem operations fail and standard thread spawning panics.
- [winit web integration](https://docs.rs/winit/0.30.13/winit/platform/web/index.html).
- [wgpu 30 feature selection](https://docs.rs/crate/wgpu/30.0.1/features).
- [WebGPU supported limits](https://developer.mozilla.org/en-US/docs/Web/API/GPUSupportedLimits).
- [web-time](https://docs.rs/web-time/latest/web_time/).
