# Fuzz targets

[cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz) (libFuzzer) targets for the code that handles untrusted input.

| Target | What it exercises | Invariants checked |
| --- | --- | --- |
| `bundle_decode` | `AreaBundle::decode` on arbitrary bytes | never panics; decoded bundles re-encode and decode again |
| `scene_json` | `SceneFile::from_json` (user-imported scene files) | never panics; accepted scenes respect `MAX_CAMERAS` / `MAX_CUSTOM_MODELS`, positions are finite, export re-imports |
| `overpass_response` | the server's Overpass parser plus `build_bundle` | never panics; the built bundle decodes |
| `bbox_query` | `BBox::parse` and helpers behind `/api/area?bbox=` | never panics; parsed boxes are ordered; cache keys contain no path separators |
| `coverage_geometry` | occlusion index, view cones and coverage rasters on arbitrary geometry (NaN, infinities, huge extents) | never panics or exhausts memory; visible cells stay inside the grid; counts return to zero |

`seeds/<target>/` holds small seed inputs (the demo street's Overpass response, a sample scene, the demo bbox). The working corpus (`corpus/`) and crash reproducers (`artifacts/`) are not committed.

## Running

Fuzzing needs nightly. `rust-toolchain.toml` in this directory pins it, so run the commands from here:

```sh
cd fuzz
cargo install --locked cargo-fuzz --version 0.13.2
cargo fuzz list
cargo fuzz run scene_json corpus/scene_json seeds/scene_json -- -max_total_time=60
```

To reproduce a crash, run `cargo fuzz run <target> artifacts/<target>/crash-...`. To print it as structured input, run `cargo fuzz fmt <target> <file>`. When you fix a finding, add a unit test next to the fix as well (see `occlusion.rs` and `coverage.rs`).

CI (`.github/workflows/fuzz.yml`) runs every target for 30 s on pull requests and pushes to `develop`, for 10 min nightly, and for a chosen duration on manual dispatch. Crashing inputs are uploaded as a workflow artifact.

## Findings so far

The `coverage_geometry` target found two bugs on its first runs, both fixed with regression tests:

- **`Occluders::new`:** the grid dimensions overflowed `i32` for footprints with huge coordinates. Without the overflow, the same input would have tried to allocate billions of grid cells. Non-finite footprints are now skipped, and the grid is capped at 512 × 512 cells (cells grow instead).
- **`facade_cells`:** a 4e18 m wall tried to allocate gigabytes of 1 m patches. Patches per wall are now capped at 256 × 128, and non-finite walls are skipped.
