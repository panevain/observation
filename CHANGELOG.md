# Changelog

All notable changes to this project are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and versions follow
[Semantic Versioning](https://semver.org/). The Prepare release workflow moves
the Unreleased entries under the new version (see CONTRIBUTING.md).

## [Unreleased]

### Added

- Opt-in Prometheus metrics (`OBS_METRICS_BIND`), JSON logs (`OBS_LOG_FORMAT=json`) and an `x-request-id` header on every response.
- Release images are signed with cosign (keyless).
- `image-full.yml` builds the in-image client path before release, and a `zizmor (workflow audit)` check (required in the rulesets: re-import them).
- Treemap workflow that refreshes the bundle-size chart for each release tag.
- `cargo-nextest` and `cargo-llvm-cov` support (`mise run coverage`), and a zizmor audit of the workflows.
- A colour-coded mode banner across the top of the 3D view (Select, Place, Aim, Move, Region) with the selected camera or the model being placed.
- Search in the camera model lists.
- Per-camera colours: each type has a default colour (further cameras of a type get lighter and darker shades), and any camera's colour can be changed.
- The demo street starts with ten example cameras.
- A Region tool (R) to outline part of the area on the ground; while it is on, the coverage figures count only what lies inside. The region can be switched off, redrawn or cleared, and is saved with the scene (scene files gain an optional `region`).

### Changed

- Image tags: releases publish only `devel`, `X.Y.Z` and `latest`. The old `edge`, `main`, `X.Y` and `sha-*` tags are no longer pushed and are removed by the Prune images workflow once `PRUNE_IMAGES=true`; pin deployments to a version tag.
- The CI Docker job reuses the wasm job's `dist/` instead of compiling the client again.
- `rust-toolchain.toml` is the only place that sets the Rust version for mise users; `mise run sync` checks the remaining copies.
- The Analysis section is last in the inspector.
- Scene files store a camera's colour as an optional `color` (`[r, g, b]`); the old `color_index` is ignored.

### Fixed

- Scrolling or clicking on the side panels no longer zooms or clicks the 3D view, and the Delete button removes the selected camera again.
- Clicks in the 3D view land under the pointer; they were offset by the tool rail and top bar.
- Large dark patches no longer flicker across the ground while zooming (the plain ground and the map tiles were fighting in the depth buffer).
- The map picker shows a stretched lower-zoom tile (or the finer tiles it has) while a tile loads, instead of an empty square.
