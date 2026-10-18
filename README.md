# Observation

[![CI](https://github.com/panevain/observation/actions/workflows/ci.yml/badge.svg)](https://github.com/panevain/observation/actions/workflows/ci.yml)
[![Security](https://github.com/panevain/observation/actions/workflows/security.yml/badge.svg)](https://github.com/panevain/observation/actions/workflows/security.yml)
[![CodeQL](https://github.com/panevain/observation/actions/workflows/codeql.yml/badge.svg)](https://github.com/panevain/observation/actions/workflows/codeql.yml)
[![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/panevain/observation/badge)](https://scorecard.dev/viewer/?uri=github.com/panevain/observation)

Observation shows how much of a suburban street common home surveillance cameras can see. Pick a street (or use the bundled demo), place cameras from a catalogue of popular doorbell, floodlight and bullet models, and the app draws what each one covers on the road and on building walls, with buildings blocking the view. It exists to make the reach of everyday home cameras easy to see and discuss.

Coverage is an estimate. It is modelled from published field of view, resolution and night-vision range, and from OpenStreetMap building footprints with inferred heights. It ignores trees, fences, vehicles and terrain. See [limitations](docs/usage.md#limitations-and-accuracy).

## Quick start

### Docker

```sh
docker build -t observation .
docker run --rm -p 8080:8080 -e OBS_CONTACT=you@example.com observation
```

Open <http://localhost:8080>. `OBS_CONTACT` goes into the `User-Agent` sent to OpenStreetMap services so they can reach you; set it to something real.

### From source

You need the toolchain pinned in [`rust-toolchain.toml`](rust-toolchain.toml) (run `rustup toolchain install` in the repository to get it, with the `wasm32-unknown-unknown` target) and [trunk](https://trunkrs.dev) 0.21.14.

```sh
trunk build --release                 # client -> dist/
cargo run --release -p observation-server
```

The server listens on `0.0.0.0:8080` and serves the `dist/` directory relative to the working directory by default (`OBS_STATIC_DIR`), so run it from the repository root. For local use, add `OBS_BIND=127.0.0.1:8080` and `OBS_CONTACT=you@example.com`. Cached map data goes to `cache/` (`OBS_CACHE_DIR`).

Dev loop: the server reads `dist/` from disk on every request, so after the first build you can rerun `trunk build` and refresh the browser without restarting the server. `trunk serve` has no API proxy configured, so use the server to test anything that loads map data. The demo street needs no network except for map tile images. Server and core crates build natively with plain `cargo` (the client is excluded from the default workspace members).

## Supported screens

Tablets and desktops with a viewport of at least 768 x 600 CSS px, in a browser with WebAssembly and WebGL2. Smaller screens see a "Larger screen needed" notice instead of the app.

## Bundle size

The shipped WebAssembly module is 26.1 MB raw and 7.8 MB gzip. See [Bundle size](docs/design.md#bundle-size) for what dominates it and how to regenerate this chart.

[![Treemap of the WebAssembly bundle, tiled by size per crate](docs/images/wasm-bundle-treemap.svg)](docs/design.md#bundle-size)

## Repository layout

| Path | Contents |
| --- | --- |
| `crates/observation-core` | Engine-free logic shared by client and server: projection, tiles, building extrusion, bundle format, cameras, view cones, occlusion, coverage, scene files |
| `crates/observation-server` | axum server: Overpass pipeline, bundle cache, tile proxy, demo street, static files |
| `crates/observation-client` | Bevy/egui client compiled to WebAssembly; embeds the camera catalogue |
| `web/` | `index.html` trunk entry point: loading screen and small-screen gate |
| `docs/` | [usage](docs/usage.md), [design](docs/design.md), [camera catalogue](docs/camera-catalog.md) |
| `scripts/` | `check-design-tokens.sh`, the colour-literal guard run in CI |
| `.github/` | CI, security, CodeQL, Scorecard and release workflows; Dependabot config |

## Documentation

- [Using the app](docs/usage.md)
- [Architecture and design](docs/design.md), including configuration, security and testing
- [Camera catalogue](docs/camera-catalog.md): sources and how to add a model
- [Security policy](SECURITY.md): supported versions and how to report a vulnerability

## Data and licensing

Code is licensed under MIT OR Apache-2.0 ([LICENSE-MIT](LICENSE-MIT), [LICENSE-APACHE](LICENSE-APACHE)). Map data and buildings are © OpenStreetMap contributors, available under the [ODbL](https://www.openstreetmap.org/copyright). The server follows the OSM tile usage policy and Overpass fair-use guidance (identifying `User-Agent`, `Referer` forwarding, caching, bounded concurrency, visible attribution); details in [docs/design.md](docs/design.md#tile-proxy-and-osm-policy-compliance). The default upstreams are public community services, so put a heavily used deployment on its own tile and Overpass providers via `OBS_TILE_URL` and `OBS_OVERPASS_URL`.
