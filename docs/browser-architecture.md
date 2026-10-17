# Browser architecture through Phase 1

The original client uses WebGPU and shared-memory Workers. Native preparation produces deterministic resource ZIPs and SHA-256 receipts. The browser verifies every bundle and expanded file, then mounts immutable resources in the shared virtual filesystem. CacheStorage reuses verified bytes across visits and evicts corrupt entries.

Mutable settings and local saves are checkpointed to OPFS through an atomic catalog of immutable blobs. Cross-tab Web Locks protect restoration and commits. Obsolete blobs are collected only when safe; live saves are capped at 512 MiB. A virtual filesystem write becomes durable only after its asynchronous checkpoint succeeds. Errors remain visible and failed batches retry.

Phases 2–4 are not implemented: browser multiplayer, active browser mods and approval portal remain future changes. Serve this branch locally with `tools/serve_web.py`; COOP/COEP headers are required.
