# Dedicated-server memory measurements

Measured locally on 2026-10-17 with the optimized `fast` native profile, prepared 26.3 resources, ordinary generated survival terrain, seed 12345, view distance 4, simulation distance 3 and eight player slots. These are engine measurements, excluding gateway, validator, browser, proxy and operating system. No flat-world substitution was used.

| Engine/configuration | Idle RSS | Startup/observed peak RSS | Tick rate | Mean tick time |
|---|---:|---:|---:|---:|
| Original loading, 15 automatic workers | 1,238 MiB | 1,238 MiB | 20.00 TPS | 0.237 ms |
| Original loading, one worker and one CPU | 1,082 MiB | 1,082 MiB | 20.00 TPS | 0.181 ms |
| Dedicated meshing disabled, one worker and one CPU | 1,079 MiB | 1,079 MiB | 20.00 TPS | 0.247 ms |
| Compact gameplay loading plus no dedicated meshing, one worker and one CPU | **556 MiB** | **602 MiB** | **20.00 TPS** | **0.269 ms** |

All rows had 81 loaded chunks and no connected players. Startup reached `Done` in 10.710 seconds for the final fresh-world run; concurrent builds affected timing, so this is not an isolated CPU benchmark. The memory reduction against the comparable one-worker baseline is about 526 MiB (49%). Limiting glibc arenas and changing trim thresholds alone saved only about 43 MiB in a separate baseline experiment.

The principal change is [the compact gameplay reader](../../../crates/horizon-world/src/level/gameplay_decode.rs), used by [the fallback behaviour loader](../../../crates/horizon-world/src/level/data.rs). World generation previously decoded the 53 MiB `gameplay-facts.json` into a generic tree containing millions of values, including geometry that the behaviour loader never used. The compact reader validates skipped geometry without retaining it and keeps the required scalar/sound fields. Tests cover defaults, wrong scalar types, duplicate keys and invalid skipped JSON. An additional test compared every behaviour field across all 35,723 prepared states against the original JSON implementation and passed. Dedicated worlds also stop scheduling render meshes in `Game::make_streaming`; lighting and simulation remain active.

The shared development server was then restarted on the same saved world with one worker and `MALLOC_ARENA_MAX=2`, reaching `Done` in 8.411 seconds. Its early RSS was 531 MiB with a 582 MiB high-water mark. Samples for the subsequent player test are recorded privately in `.local/game-dev/memory-measurements.jsonl`. The fresh-world comparison above was constrained to one CPU; the shared browser-test process has one worker without CPU affinity.

A 1 GiB host now has plausible idle headroom, but these results alone do not establish capacity with players, exploration, mods or concurrent validation. Keep one generation worker (`workers:1` in `horizon-graphics.txt`), measure the complete production process set, and retain swap as a buffer rather than treating it as available game RAM. Two-player and deployed-host results must be added before claiming that the small host sustains the target workload.
