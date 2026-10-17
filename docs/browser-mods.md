# Phase 3 browser and native server components

The browser host runs each validated component in a dedicated Worker through Jco 1.39.0's generated bindings and the browser WebAssembly engine. Existing Rust game hooks exchange snapshots and bounded output through the browser facade. Native server components still run in Wasmtime; server clock/chat/console hooks and a private approved-file mirror are integrated. Remote clients now call the tick hook, so coords-hud receives position updates.

Build `horizon-mod-validator` natively and install the pinned Jco dependencies in `web/mods`. Run `HORIZON_JCO=/absolute/path/to/jco horizon-mod-validator --input /private/coords_hud.hmod --browser-out /private/empty-output` with a process deadline. The JSON result contains the manifest, validation checks and each generated artifact's size/hash. Validation includes archive paths and sizes, manifest/import capabilities, content fence, native initialization fuel, browser memory caps of 64 MiB and tables of 10,000 entries. Registry additions require a coordinated release and are rejected for this live path.

For this phase, an operator installs the checked artifact directory as private `bundles/<package-sha256>/` and writes `index.json` before starting the gateway. Each index mod contains `id`, `sha256`, `side`, full `manifest`, `entry` as `/mods/bundles/<hash>/mod.js`, and `files` with the receipt's `path`, `sha256`, `size`, plus matching same-origin `url`. The catalog is loaded and all artifact hashes verified at startup. `schema` is 1 and `revision` a nonnegative integer. Set `modsRuntimeDir` to the public runtime containing host.js and worker.js. No uploaded JavaScript is accepted. The catalog allows eight browser mods and 32 total entries.

For server/both components, the operator's separate private `HORIZON_APPROVED_MODS` directory holds `<id>.hmod`. The engine initializes a candidate before replacing its previous live instance and keeps the last good mod after failure. The server has no local-player snapshot.

The browser checks artifacts again before execution, provides explicit WIT capability imports, serializes callbacks and terminates Workers that exceed wall-clock deadlines. It does not claim native instruction-fuel equivalence. WASI and arbitrary filesystem/network imports are not supplied.

The upload, owner review, atomic publication and automatic live notification workflows are not implemented until Phase 4. This branch's catalog is operator-curated and readonly while the gateway runs.

Isolated Phase 3 validation: native client/server/validator check and nightly browser atomics/build-std check passed; four gateway/catalog tests passed. Browser-rendered coords-hud evidence belongs to the complete integration branch and is recorded separately from these compile checks.
