# Changelog

All notable changes to this project are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and versions follow
[Semantic Versioning](https://semver.org/). The Prepare release workflow moves
the Unreleased entries under the new version (see CONTRIBUTING.md).

## [Unreleased]

### Added

- Local map data: an optional region agent (`observation-regions`, Docker target `regions`, run together with the app by the new `compose.yaml`) imports Geofabrik extracts into its own Overpass databases. Areas inside an imported region load from it instead of public Overpass, which stays the fallback elsewhere. A **Local map data** dialog (database button in the top bar) lists regions, imports new ones from a searchable list, and updates, cancels or deletes them, with live progress over server-sent events; the top bar shows a running import and a message says when it is done. Cached bundles inside a region are rebuilt after each import, and regions refresh every 7 days. Set `OBS_REGIONS_URL` and `OBS_REGIONS_TOKEN` to enable it and `OBS_ADMIN_TOKEN` to allow management from the UI; the map picker outlines regions with local data.

- Opt-in Prometheus metrics (`OBS_METRICS_BIND`), JSON logs (`OBS_LOG_FORMAT=json`) and an `x-request-id` header on every response.
- Release images are signed with cosign (keyless).
- `image-full.yml` builds the in-image client path before release, and a `zizmor (workflow audit)` check (required in the rulesets: re-import them).
- Treemap workflow that refreshes the bundle-size chart for each release tag.
- `cargo-nextest` and `cargo-llvm-cov` support (`mise run coverage`), and a zizmor audit of the workflows.
- A mode pill at the top left of the 3D view (Select, Place, Aim, Move, Region) with what a click does and the selected camera or the model being placed; the pointer changes with the tool, and the tool rail has icons.
- Search in the camera model lists.
- Per-camera colours: each type has a default colour (further cameras of a type get lighter and darker shades), and any camera's colour can be changed.
- The demo street starts with ten example cameras.
- A Region tool (R) to outline part of the area on the ground; while it is on, the coverage figures count only what lies inside. The region can be switched off, redrawn or cleared, and is saved with the scene (scene files gain an optional `region`).
- Settings menu (gear in the top bar) with a choice of UI font, Inter or Iosevka, remembered in the browser. Both are bundled (subset to Latin-1, about 100 KB together; `scripts/subset-fonts.sh` rebuilds them) and figures use tabular digits.
- Phosphor icons; each camera type has its own icon in the camera list and model picker.
- A toolbar floats above the selected camera in the 3D view (Aim, Move, Focus, Duplicate, Delete), and camera rows have a right-click menu with the same actions plus Include in coverage.
- The status bar lists the active tool's keys as keycaps and shows the three coverage figures; the tool rail shows each tool's shortcut.
- Hovering a camera's row highlights it in the 3D view, and hovering a camera in the view highlights its row.
- A compass dial for a camera's heading, with its field of view drawn as a wedge; Ctrl (or Cmd) makes number fields drag ten times faster (Shift already slows them).
- Undo and redo (top-bar arrows, Ctrl/Cmd+Z, Ctrl/Cmd+Shift+Z, Ctrl/Cmd+Y) for camera edits, deletes, the region, analysis settings, custom models, imports and area changes; a drag or typed name is one step. Deleting cameras or loading an area over them shows a message with an Undo button.
- Failed area loads, imports and restores stay on screen until dismissed, and a failed area load has a Retry button; a repeated message replaces the earlier copy.
- The loading card shows how long the request has taken, explains slow first loads, and offers Cancel and Use demo street instead.
- A "No cameras yet" empty state with a Place camera button replaces the 0% figures.
- The status bar shows whether the autosave reached browser storage (Saved, or Not saved with a hint to export).

### Changed

- Docker Compose settings no longer use `.env`; `.env.example` is gone and `mise run up` starts the stack. `OBS_REGIONS_TOKEN` and `OBS_ADMIN_TOKEN` are [fnox](https://fnox.jdx.dev) secrets declared in `fnox.toml`, with values in the gitignored `fnox.local.toml`; other settings (`OBS_PORT`, `OBS_LOG_FORMAT`, region limits) go in the gitignored `mise.local.toml`. Move your values out of `.env`, or export them yourself before `docker compose up`.
- Upstream hosts on private networks need `OBS_ALLOW_PRIVATE_UPSTREAMS=1` (server and region agent). `OBS_ALLOW_PRIVATE_UPSTREAMS` accepts `1`/`true`/`yes`/`on` and `0`/`false`/`no`/`off`; other values stop the server at startup.
- Error messages say what went wrong and what to do next: failed area loads tell offline from a down server, a server error or damaged data; storage, import and export failures give the browser's reason (the status bar tooltip says why the autosave failed); scene, bundle, camera-model and bounding-box errors name the field and the allowed range; the server's replies name the limit (for example the size of a too-large box) and never include internal details.
- Image tags: releases publish only `devel`, `X.Y.Z` and `latest`. The old `edge`, `main`, `X.Y` and `sha-*` tags are no longer pushed and are removed by the Prune images workflow once `PRUNE_IMAGES=true`; pin deployments to a version tag.
- The CI Docker job no longer compiles the client: it builds the image around a stub `dist/` (as the Trivy scan does) and runs alongside the wasm build instead of after it, which takes the Docker build off the critical path.
- CI writes Rust and Docker layer caches from `develop` only (no longer from `main` or pull requests), keeps line tables instead of full debug info, caches trunk's wasm-bindgen and wasm-opt downloads, and no longer cancels branch runs: only a newer push to the same pull request cancels a run, so every commit on `develop`, `main`, `release/**` and `hotfix/**` is checked. Tests run under a nextest `ci` profile (`.config/nextest.toml`) that reports every failure and ends a test hung for 3 minutes. Fuzzing runs on pull requests only when the fuzzed crates or `fuzz/` change.
- `rust-toolchain.toml` is the only place that sets the Rust version for mise users; `mise run sync` checks the remaining copies.
- `OBS_OVERPASS_URL` takes up to 4 comma-separated interpreters, tried in order; malformed entries stop the server at startup. The whole-request timeout is 80 s (was 60 s) and an Overpass attempt 35 s (was 40 s).
- The Analysis section is last in the inspector.
- Scene files store a camera's colour as an optional `color` (`[r, g, b]`); the old `color_index` is ignored.
- Camera type colours are now cool and neutral (doorbell blue, floodlight green, spotlight sky blue, bullet indigo, turret teal, dome violet, indoor slate) so cones never match a step of the yellow-to-purple coverage ramp; a type gets two extra shades before they repeat. Saved per-camera colours are unchanged.
- The selection and aim markers are white over a dark casing, and the region outline is dark, so none of them reads as a camera or coverage colour.
- Flatter widget style: buttons show hover and press by fill instead of an outline, input fields have a 3:1 edge, corners are smaller (4/6/8 px) and floating cards and menus have a shadow.
- Selected camera is grouped into a name header, Position, Orientation and a collapsed Optics section; the face vector moved to the compass label's tooltip. Camera rows use an eye toggle instead of a checkbox, and the Cones and Walls toggles look different from the tools.

### Fixed

- Loading an area no longer fails with 502 when Overpass answers 502/503 once or does not answer in time: transient failures are retried (on the next interpreter, or once on a lone one) within the request's time budget. An interpreter that answers 429/504 rests for 30 s on its own, and a final timeout is a 504 with a clear message.
- Scrolling or clicking on the side panels no longer zooms or clicks the 3D view, and the Delete button removes the selected camera again.
- Clicks in the 3D view land under the pointer; they were offset by the tool rail and top bar.
- Large dark patches no longer flicker across the ground while zooming (the plain ground and the map tiles were fighting in the depth buffer).
- The map picker shows a stretched lower-zoom tile (or the finer tiles it has) while a tile loads, instead of an empty square.

### Security

- Upstream requests (tile server, Overpass, region downloads) go through a resolver that refuses loopback, private, link-local and similar addresses, including names that resolve to them and IPv6 forms that embed an IPv4 address (NAT64, 6to4, mapped). `OBS_OVERPASS_URL`, `OBS_TILE_URL` and `OBS_REGIONS_CATALOG_URL` are also checked at startup.
- The server no longer follows redirects from the tile and Overpass upstreams, and the region agent follows them only within the original host, so an upstream cannot send them to an internal address. A 3xx from Overpass now fails over to the next interpreter and is logged with its `Location`.
- Proxy environment variables (`HTTP_PROXY`, `HTTPS_PROXY`) are ignored for upstream requests unless `OBS_ALLOW_PRIVATE_UPSTREAMS=1`, since a proxy resolves names past the filter.
- Credentials in upstream URLs are removed from startup errors.
