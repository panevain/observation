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
- Settings menu (gear in the top bar) with a choice of UI font, Inter or Iosevka, remembered in the browser. Both are bundled (subset to Latin-1, about 100 KB together; `scripts/subset-fonts.sh` rebuilds them) and figures use tabular digits.
- Phosphor icons; each camera type has its own icon in the camera list and model picker.

### Changed

- Image tags: releases publish only `devel`, `X.Y.Z` and `latest`. The old `edge`, `main`, `X.Y` and `sha-*` tags are no longer pushed and are removed by the Prune images workflow once `PRUNE_IMAGES=true`; pin deployments to a version tag.
- The CI Docker job reuses the wasm job's `dist/` instead of compiling the client again.
- `rust-toolchain.toml` is the only place that sets the Rust version for mise users; `mise run sync` checks the remaining copies.
- `OBS_OVERPASS_URL` takes up to 4 comma-separated interpreters, tried in order; malformed entries stop the server at startup. The whole-request timeout is 80 s (was 60 s) and an Overpass attempt 35 s (was 40 s).
- The Analysis section is last in the inspector.
- Scene files store a camera's colour as an optional `color` (`[r, g, b]`); the old `color_index` is ignored.
- Camera type colours are now cool and neutral (doorbell blue, floodlight green, spotlight sky blue, bullet indigo, turret teal, dome violet, indoor slate) so cones never match a step of the yellow-to-purple coverage ramp; a type gets two extra shades before they repeat. Saved per-camera colours are unchanged.
- The selection and aim markers are white over a dark casing, and the region outline is dark, so none of them reads as a camera or coverage colour. Tool colours are darker so banner text meets 4.5:1 contrast.
- Flatter widget style: buttons show hover and press by fill instead of an outline, input fields have a 3:1 edge, corners are smaller (4/6/8 px) and floating cards and menus have a shadow.

### Fixed

- Loading an area no longer fails with 502 when Overpass answers 502/503 once or does not answer in time: transient failures are retried (on the next interpreter, or once on a lone one) within the request's time budget. An interpreter that answers 429/504 rests for 30 s on its own, and a final timeout is a 504 with a clear message.
- Scrolling or clicking on the side panels no longer zooms or clicks the 3D view, and the Delete button removes the selected camera again.
- Clicks in the 3D view land under the pointer; they were offset by the tool rail and top bar.
- Large dark patches no longer flicker across the ground while zooming (the plain ground and the map tiles were fighting in the depth buffer).
- The map picker shows a stretched lower-zoom tile (or the finer tiles it has) while a tile loads, instead of an empty square.
