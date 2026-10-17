# Verified resources and browser persistence

Run native `horizon-prepare` for the intended version, then `python3 web/prepare-assets.py --help` for deterministic resource packaging. The client loads the generated `/assets/index.json` pointer and verifies its content-addressed index, bundles and individual files before the game starts. Cache entries are untrusted and checked again before use.

Settings and local saves are restored from OPFS before startup. Periodic checkpoints commit immutable blobs first and atomically replace the catalog last. Shared restore locks and exclusive write/collection locks prevent another tab from deleting files in use. Failed batches retain their bytes for retry and display a storage warning. The browser window never blocks on region writer or generation Worker completion. Let a successful checkpoint finish before closing the page.

The live-save budget is 512 MiB. Game resources are immutable and cannot be overwritten through saves. Hosted server worlds are not part of this phase; multiplayer arrives separately.

Isolated Phase 1 validation: native client/server `cargo check` and the nightly browser atomics/build-std check passed.

Optional audio is prepared after native horizon-prepare with `python3 web/prepare-sounds.py --download`, then `python3 web/prepare-assets.py`. Omitting `--download` verifies and reuses cached objects without network. Preparation verifies the pinned manifest/index; pinned source sizes/hashes are verified natively, a small SHA-256 sound manifest joins the startup resources, and encoded sounds load on demand through authenticated same-origin fetch. Startup ZIPs exclude sound blobs; the bounded browser cache verifies sounds before reuse.
