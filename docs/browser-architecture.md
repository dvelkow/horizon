# Browser follow-on phases

Phase 0 introduces the platform adapters and real title bootstrap. Phase 1 will add verified resource caching and OPFS persistence. Phase 2 will add an authenticated fixed-destination WebSocket-to-TCP gateway for the dedicated engine. Phase 3 will execute capped mod components through the browser WebAssembly engine and retain native Wasmtime server mods. Phase 4 will add bounded uploads, checks, explicit owner approval of exact SHA-256 hashes, atomic publication, and live update notifications.

Phases 1–4 are not implemented in this branch. Cloud credentials belong only in private operator tooling; they must never be placed in served directories or browser assets.
