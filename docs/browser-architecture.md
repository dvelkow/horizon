# Browser architecture through Phase 3

The existing client runs through WebGPU and shared-memory Workers with verified resources, OPFS settings and authenticated WebSocket-to-TCP multiplayer. Browser components use Jco bindings with dedicated browser Workers; native server components retain Wasmtime. Both receive the existing API 0.1 hooks and retain last-good instances during replacement.

This phase serves only a readonly operator-installed mod catalog whose artifact hashes are checked before service and again in the browser. See [component setup and boundaries](browser-mods.md). Phase 4 upload, exact-hash owner approval and atomic live publication remain unimplemented.
