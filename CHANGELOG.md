# Changelog

All notable changes to this project are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and versions follow
[Semantic Versioning](https://semver.org/). The Prepare release workflow moves
the Unreleased entries under the new version (see CONTRIBUTING.md).

## [Unreleased]

### Added

- Cameras are grouped by street address in the camera list, worked out from OpenStreetMap: a camera belongs to the nearest building or address point with an address within 25 m. Each group has a heading with the address and its camera count. A camera's new **Group** field overrides its address with a typed name or another nearby address (scene files gain an optional `group` per camera). Bundles carry building addresses and address nodes (bundle format version 2, so cached bundles are rebuilt), the Overpass query also asks for address nodes, and the region agent keeps them when importing (existing regions gain them at their next refresh).
- Local map data: an optional region agent (`observation-regions`, Docker target `regions`, run together with the app by the new `compose.yaml`) imports Geofabrik extracts into its own Overpass databases. Areas inside an imported region load from it instead of public Overpass, which stays the fallback elsewhere. A **Local map data** dialog (database button in the top bar) lists regions, imports new ones from a searchable list, and updates, cancels or deletes them, with live progress over server-sent events; the top bar shows a running import and a message says when it is done. Cached bundles inside a region are rebuilt after each import, and regions refresh every 7 days. Set `OBS_REGIONS_URL` and `OBS_REGIONS_TOKEN` to enable it and `OBS_ADMIN_TOKEN` to allow management from the UI; the map picker outlines regions with local data.

- Custom camera models can be edited and deleted: pick one under **Model** in the Custom model form to change its values (cameras that use it keep it and update) or delete it. A model that cameras still use cannot be deleted, and the message says how many use it. New custom models get an id no other model has, and the form goes on to edit a model once it is added. A scene holds at most 64 custom models, so adding one past the limit is refused instead of autosaving a scene that cannot be restored.
- A **Custom** detail level with a user-entered pixel density (1-2000 px/m, saved in scenes as `params.custom_ppm`) for calibrating ranges against real footage.
- Opt-in Prometheus metrics (`OBS_METRICS_BIND`), JSON logs (`OBS_LOG_FORMAT=json`) and an `x-request-id` header on every response.
- Release images are signed with cosign (keyless).
- `image-full.yml` builds the in-image client path before release, and a `zizmor (workflow audit)` check (required in the rulesets: `mise run rulesets:apply`).
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
- `mise run down` stops and removes the Docker Compose stack, keeping the region and cache volumes (`mise run down -v` deletes them too).
- The region agent image is published as `ghcr.io/panevain/observation-regions` (signed and attested like the app image), and `mise run up` now runs the released images (`OBS_VERSION`, default `latest`) instead of building them; `mise run up --build` builds both from the checkout as before.
- Each release gets a GitHub release with its changelog section and both image digests.
- Security patch releases: Release watch checks the latest release every day, and when a dependency or base-image fix can be shipped, Hotfix opens the patch release, which is published after one approval. Release tag tags `main` and opens the back-merge into `develop` for every release, and Pins opens a weekly PR for the pinned tools Dependabot does not cover. They act as a GitHub App (setup in CONTRIBUTING.md).

### Changed

- Images are based on Debian 13: distroless `cc-debian13` for the app and `debian:trixie-slim` for the region agent (the Debian 12 distroless images are no longer rebuilt). The server and region agent binaries are built with cargo-auditable, so image scanners and the SBOM see their crates.
- Rust 1.99.1 and trunk 0.22.0.
- Tooling is driven from mise. The tools only CI and the image build use (Trivy, cosign, cargo-audit, cargo-deny, zizmor, cargo-nextest, cargo-llvm-cov, cargo-fuzz, cargo-chef and cargo-auditable) are pinned in `mise.ci.toml` and locked with their checksums in `mise.ci.lock`, next to CI's environment and the security, fuzz and scan steps as tasks (`mise -E ci run <task>` runs one locally). The Dockerfile installs trunk, cargo-chef and cargo-auditable from the lockfiles, and the Pins workflow bumps the mise tools with `mise upgrade --bump`, each a week after its release. `mise run fonts` rebuilds the UI fonts with pinned fonttools and opentype-feature-freezer.
- Cameras are easier to click and tap: a press that misses every camera picks the nearest one within 14 px (22 px for a tap), unless a building hides it. Hovering uses the same reach. The Place tool still needs a direct hit, so a camera can be placed next to another.
- Docker Compose settings no longer use `.env`; `.env.example` is gone and `mise run up` starts the stack. The Compose tokens (`OBS_REGIONS_TOKEN`, `OBS_ADMIN_TOKEN`) are [fnox](https://fnox.jdx.dev) secrets declared in `fnox.toml`, with values in the gitignored `fnox.local.toml`; fnox is installed by mise for that task only. The other Compose settings (`OBS_PORT`, `OBS_LOG_FORMAT`, region limits) are set on the `up` task in `mise.toml` and can be overridden per run, e.g. `OBS_PORT=9090 mise run up`. `OBS_CONTACT` still comes from `mise.local.toml`, where it is now commented out by default. Move your values out of `.env`, or export them yourself before `docker compose up`.
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

- Ctrl/Cmd+V pastes into text fields such as the admin token, and Ctrl/Cmd+C and X copy and cut from them. The browser's clipboard events were not wired up, and Bevy cancelled the key presses that fire them.
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
