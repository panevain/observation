# AGENTS.md

Guidance for AI coding agents working in this repository. It points to the real documentation rather than repeating it; where the two differ, the documentation wins.

## What it is

Observation shows how much of a suburban street common home surveillance cameras can see. A Bevy client compiled to WebAssembly computes camera coverage in the browser, on the CPU. An axum server fetches OpenStreetMap buildings through Overpass, adds elevation, turns them into a compact binary bundle, proxies map tiles and serves the client. An optional region agent imports Geofabrik extracts into local Overpass databases. See [README.md](README.md) and the [overview in docs/design.md](docs/design.md#overview).

## Architecture

A Cargo workspace under `crates/` ([Workspace in docs/design.md](docs/design.md#workspace) has the module lists):

- `observation-core`: engine-free logic shared by client and server (projection, tiles, extrusion, bundle format, cameras, view cones, occlusion, coverage, scene files).
- `observation-mapdata`: map data without I/O: the Overpass response parser, Terrarium elevation tile decoding and the demo street built from the committed fixtures.
- `observation-server`: the axum server binary (Overpass pipeline, bundle cache, tile proxy, elevation, demo street, region relay, static files).
- `observation-regions`: the optional region agent binary (Geofabrik import into local Overpass, queries).
- `observation-regions-api`: the HTTP contract between the server and the region agent.
- `observation-upstream`: every outbound HTTP request of the server and the agent, behind SSRF guards.
- `observation-tokens`: design token values, and the `token-css` binary that trunk runs to write `web/tokens.css`.
- `observation-client`: the Bevy/egui app for `wasm32-unknown-unknown` and its coverage worker. Not a default member.
- `observation-client-model`: the client's engine-free logic (no Bevy, egui or `web-sys`).

Two separate workspaces, each with its own `Cargo.lock`: `xtask/` (repository tooling, `cargo xtask <command>`) and `fuzz/` (cargo-fuzz targets, nightly only). `web/index.html` is trunk's entry point (`Trunk.toml`).

## Tech stack

Rust, edition 2024, with the toolchain pinned in `rust-toolchain.toml`. Bevy with bevy_egui, axum and tokio, reqwest with rustls, postcard for the bundle wire format, and trunk for the wasm build. mise installs every tool from a lockfile and defines the tasks (`mise.toml`, `mise.dev.toml`, `mise.ci.toml`): cargo-nextest, cargo-deny (`deny.toml`), typos (`.typos.toml`) and cargo-shear. Images build from `Dockerfile`; `compose.yaml` runs the app and the region agent.

## Checking your work

Run the mise tasks that CI runs, as `mise run <task>`:

- `fmt`, `clippy-native`, `clippy-wasm`, `test`, `typos`, `shear`, `deny`, `tokens`, `sync`
- `check`: all of the above plus `tasks:validate`, in order, stopping at the first failure

`test` builds the client natively and `clippy-wasm` checks it for wasm, so both (and therefore `check`) compile Bevy; `build`, `serve` and `dev` run trunk. These builds are heavy. While iterating, scope to one crate with `cargo nextest run -p <crate>` or `cargo clippy -p <crate>`. Plain `cargo build` and `cargo test` leave the client out.

## Rules and constraints

- Workspace lints forbid `unsafe` code and warn on `unwrap`, `dbg!` and `todo!` (`clippy.toml` allows `unwrap` and `expect` in tests). Clippy runs with `-D warnings`.
- Only `observation-upstream` may depend on reqwest, so that all outbound HTTP passes its SSRF guards; `deny.toml` enforces this. TLS is rustls only.
- A crate version added to any `Cargo.lock` must have been on crates.io for 7 days (`.config/cargo-cooldown.toml`).
- Colours go through `crates/observation-client/src/theme.rs` and `observation-tokens`; `mise run tokens` fails on colour literals elsewhere in the client.
- Test fakes sit behind a `testing` cargo feature, which only `[dev-dependencies]` enable.
- `OBS_BLESS=1 cargo nextest run -p <crate>` rewrites golden files; review the diff before committing. `mise run test` refuses to run with `OBS_BLESS` set.
- Git-flow: branch from `develop`, and add user-visible changes under `[Unreleased]` in `CHANGELOG.md`.
- Write plain, precise prose with British spelling, in code comments and docs alike.

## Read more

- [README.md](README.md): quick start and repository layout
- [docs/design.md](docs/design.md): architecture, configuration, security, testing and repository tooling
- [docs/usage.md](docs/usage.md): using the app, limitations and accuracy
- [docs/camera-catalog.md](docs/camera-catalog.md): camera data and how to add a model
- [CONTRIBUTING.md](CONTRIBUTING.md): branching, required checks, golden files and mise tools
- [SECURITY.md](SECURITY.md): supported versions and reporting a vulnerability
- [fuzz/README.md](fuzz/README.md) and [supply-chain/README.md](supply-chain/README.md): fuzzing and cargo-vet
