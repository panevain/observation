# Design

This document explains how Observation is built and why. For how to use the app, see [usage.md](usage.md). The camera data is documented in [camera-catalog.md](camera-catalog.md).

## Overview

Observation answers one question: given some cameras on a street, what can they see? Two decisions shape everything else.

1. **Heavy geometry work happens once, on the server.** The server turns OpenStreetMap data into a compact binary bundle (flat-roofed building prisms, a ready-made triangle mesh, road centrelines). The browser never parses OSM.
2. **Coverage is computed in the browser, on the CPU, from the same data the user sees.** Moving a camera needs no round trip, and the statistics and the coloured overlay come from the same line-of-sight results.

```
 Browser (WebAssembly)                 observation-server (axum)                 Upstream
+---------------------------+  GET /api/area?bbox=...   +----------------------+   POST (Overpass QL)   +-------------------+
| Bevy scene + egui panels  | ------------------------> | validate, snap, cache | ---------------------> | Overpass API      |
|                           | <------------------------ | single-flight, parse, |                        +-------------------+
| observation-core:         |   OBSB bundle (postcard)  | extrude, encode       |
|  view cones, occlusion,   |                           |                       |   GET tile (cached)    +-------------------+
|  coverage, scene files    |  GET /api/tiles/z/x/y.png | tile proxy + disk     | ---------------------> | OSM tile server   |
|                           | ------------------------> | cache                 |                        +-------------------+
|  localStorage autosave    |  GET /api/demo            |                       |
+---------------------------+  GET /  (static client)   | static files (dist/)  |
                                                        +----------------------+
```

The browser only ever talks to its own origin. Upstream services are reached by the server alone, which is what lets it cache, rate-limit and identify itself to them.

## Workspace

| Crate | Responsibility |
| --- | --- |
| `observation-core` | Pure logic with no engine dependency, shared by both sides. `geo` (projection, bounding boxes), `tiles` (slippy-map maths), `building` (height inference, extrusion), `bundle` (wire format, road widths), `camera` (models, DORI, catalogue parsing), `frustum` (view cones), `occlusion` (line of sight), `coverage` (raster, statistics), `scene` (saved files) |
| `observation-server` | `config` (environment), `overpass` (client and parser), `bundles` (cache plus single-flight), `cache` (disk), `tiles` (proxy), `demo` (fixture bundle), `routes/*`, `error` |
| `observation-client` | Bevy app compiled to `wasm32-unknown-unknown`: networking, 3D world, camera bodies, coverage systems, input, persistence, egui UI, design tokens |

Core and server are the workspace's default members, so plain `cargo build` and `cargo test` do not compile the Bevy client. Workspace lints forbid `unsafe` code and warn on `unwrap`, `dbg!` and `todo!`; CI turns warnings into errors.

## Coordinate frame

All local geometry is in a **world frame in metres** that matches Bevy's convention:

- `+X` east, `+Y` up, `-Z` north (so `+Z` is south).
- The origin is the **centre of the loaded bounding box**.
- Footprints are `[x, z]` pairs in the same frame.
- Camera `yaw` is a compass bearing, clockwise from north, so the forward vector is `(sin(yaw) cos(pitch), sin(pitch), -cos(yaw) cos(pitch))`. Position is stored as `[east, up, south]`.

Projection is a local **equirectangular** (plate carree) projection about the origin: `x = dlon * R * cos(lat0)`, `z = -dlat * R`, with R = 6,371,008.8 m (the IUGG mean radius). It is exact at the origin and its scale error grows with distance. Areas are limited to 500 m per side, so points are at most about 250 m from the origin: the east-west scale error there is roughly `tan(lat0) * 250 m / R`, which is about 4 x 10^-5 at 47 degrees latitude (around a centimetre at the edge) and still only about a decimetre at the 85 degree latitude limit the API accepts. That is far below the error from OSM positions and heights, and it keeps the maths trivial, invertible and cheap. A full UTM or ENU conversion would add nothing visible at this scale.

Map tiles are Web Mercator. Each tile's corner coordinates are run through the same local projection and the tile is drawn as a quad between them, which is accurate because a tile spans at most a few hundred metres of ground at the zooms used, over which Mercator's latitude distortion is a few centimetres.

## Server

### Endpoints

| Route | Response |
| --- | --- |
| `GET /healthz` | `ok` (liveness probe; the image has no `HEALTHCHECK`, so orchestrators should probe this) |
| `GET /api/config` | JSON: tile URL template for the proxy, attribution, `max_bbox_side_m`, `demo_available`, version. The client reads the tile attribution from it at startup |
| `GET /api/area?bbox=south,west,north,east` | Bundle, `Cache-Control: public, max-age=86400` |
| `GET /api/demo` | Demo bundle built at startup, `max-age=3600`; 404 if the fixture failed to build |
| `GET /api/tiles/{z}/{x}/{y}.png` | Proxied tile, `max-age=604800` |
| `/api/*` (anything else) | JSON 404, deliberately not the SPA page |
| everything else | Static client from `OBS_STATIC_DIR`, with `index.html` as fallback. HTML is `no-cache`; other files are treated as content-hashed (trunk `filehash = true`) and get `public, max-age=31536000, immutable` |

Errors are JSON `{"error": "..."}`: 400 bad request, 404, 502 upstream failure, 503 rate limited (with `Retry-After`), 500 internal (detail logged, never sent), 408 on request timeout.

### Configuration

Read from the environment at start-up; empty values count as unset; invalid `OBS_BIND` or `OBS_BUNDLE_TTL_DAYS` stop the server with an error.

| Variable | Default | Meaning |
| --- | --- | --- |
| `OBS_BIND` | `0.0.0.0:8080` | Listen address |
| `OBS_STATIC_DIR` | `dist` | Built client directory (relative to the working directory). A warning is logged if it has no `index.html` |
| `OBS_CACHE_DIR` | `cache` | Root of the bundle and tile caches |
| `OBS_CONTACT` | `https://github.com/panevain/observation` | Put in the `User-Agent` (`observation/<version> (+<contact>)`); set your own |
| `OBS_OVERPASS_URL` | `https://overpass-api.de/api/interpreter` | Overpass interpreter |
| `OBS_TILE_URL` | `https://tile.openstreetmap.org/{z}/{x}/{y}.png` | Upstream tile template (`{z}`, `{x}`, `{y}`; `{s}` becomes `a`) |
| `OBS_TILE_ATTRIBUTION` | `© OpenStreetMap contributors` | Attribution embedded in bundles and `/api/config` |
| `OBS_BUNDLE_TTL_DAYS` | `7` | Bundle freshness; fractional values allowed, `0` revalidates every time |
| `RUST_LOG` | `info` | `tracing` filter |

Fixed in code (not configurable): tile minimum freshness 7 days, tile cache cap 512 MiB, at most 4 concurrent upstream tile fetches, Overpass retry pause 2 s and cooldown 30 s, Overpass client timeout 40 s, tile client timeout 20 s, whole-request timeout 60 s, request body limit 16 KiB. The container sets `OBS_BIND=0.0.0.0:8080`, `OBS_STATIC_DIR=/app/dist` and `OBS_CACHE_DIR=/var/cache/observation`. The cache directory is not a volume; mount one there (writable by uid 65532) to keep it across container runs.

### Bundle pipeline

For `GET /api/area`:

1. **Validate.** `bbox` must be four finite numbers in Overpass order; latitudes within +/-85, longitudes within +/-180; south < north and west < east. Each side, measured at the centre latitude, must be 20-500 m. Anything else is a 400. (Boxes crossing the antimeridian are not supported.)
2. **Snap.** The box is expanded outward to a 0.0001 degree grid (about 10 m) so nearby requests share a cache entry. The cache key is `south_west_north_east` with five decimals. The bundle's origin is the snapped box's centre, and snapping is deterministic, so a saved scene that stores the requested box reproduces the same frame.
3. **Cache.** `bundles/<key>.obsb` on disk; fresh while its modification time is within the TTL.
4. **Single-flight.** Concurrent requests for one key share a single in-flight job. The job runs as its own task, so it finishes and fills the cache even if every waiting client disconnects.
5. **Fetch.** One Overpass query per box (below). Only one Overpass request is in flight at a time (a semaphore with one permit), and at most 3 more may wait for it; further misses fail fast with 503 and `Retry-After: 10`. On 429 or 504 the server pauses 2 s and retries once; if that fails it starts a 30 s **cooldown** during which new fetches fail immediately with 503 and `Retry-After`. Connection errors and other statuses are 502. A response whose `Content-Length` exceeds 32 MiB is refused. A 200 response whose `remark` reports a runtime error is treated as a failure.
6. **Parse** the `out geom` JSON (below).
7. **Build.** Project to the world frame, extrude buildings, pack, encode.
8. **Store and reply.** The bundle is written atomically (temp file in the same directory, then rename). A failed write is logged and the response is still sent.
9. **Stale on error.** If the fetch fails and an expired bundle exists, the expired bundle is served and a warning is logged.

The query is the same for every box:

```
[out:json][timeout:25];
(
  way["building"](s,w,n,e);
  relation["building"]["type"="multipolygon"](s,w,n,e);
  way["highway"](s,w,n,e);
);
out geom qt;
```

**Parsing.** Closed `building` ways with at least four points and no missing nodes become buildings. Ways that are `outer` members of a building relation are skipped, because the relation covers them. Multipolygon relations have their outer and inner member ways **stitched** end to end (reversing where needed; shared nodes have identical coordinates, so exact comparison is correct) into closed rings. Unclosed fragments are dropped. Each outer ring becomes a building, and each inner ring becomes a hole of the outer ring that contains its first point. A relation's buildings carry the negated relation id so they cannot clash with way ids. `highway` ways become roads when the type has a known width; `area=yes` ways are skipped.

**Height inference** (`building::infer_heights`), in priority order:

1. `height` (a length such as `12`, `12 m`, `40 ft`, `12'6"`), if above 0.5 m.
2. `building:levels` x 3 m per storey, with `roof:levels` added to the level count; when no roof levels are tagged, 1 m is added for the roof.
3. A default by `building=*` value: 3 m for shed, garage(s), carport, hut, roof, kiosk, cabin; 4 m for bungalow and static caravan; 6.5 m for house, detached, semidetached house, terrace, residential, farm; 10 m for apartments, dormitory, hotel; 12 m for church and cathedral; 6 m otherwise.

The result is clamped to 1-300 m. `min_height`, or `building:min_level` x 3 m, gives the base of building parts (kept at least 0.5 m below the top).

**Road widths** (`bundle::road_width_m`): a `width` tag between 1 and 60 m wins; else `lanes` (1-11) x 3.25 m; else a default by type: motorway and trunk 14 m, primary 10, secondary 8, tertiary 7, residential and unclassified 6, living street 5, service 4, footway, path, pedestrian, cycleway and steps 2, `*_link` ramps 6. Other highway types are ignored.

**Extrusion.** Each footprint becomes vertical walls with outward normals (separate vertices per wall face, so shading stays flat) and a flat roof triangulated with `earcut`, holes included. Raised parts also get an underside. Footprints that are degenerate (fewer than three distinct points, near-zero area, or no height) are dropped. Roofs are always flat: this is what makes the occlusion test exact (see below).

### Bundle format

A response body is:

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 4 bytes | Magic `OBSB` |
| 4 | 2 bytes | Format version, `u16` little-endian (currently `1`) |
| 6 | rest | `AreaBundle`, encoded with [postcard](https://docs.rs/postcard) |

`AreaBundle` fields, in order: `bbox` (south, west, north, east as f64), `origin` (lat, lon), `buildings`, `roads`, `mesh`, `attribution`, `generated_unix`.

| Type | Fields |
| --- | --- |
| `Building` | `osm_id` (i64; negative for relations), `footprint` (outer ring, holes, `min_height_m`, `height_m`; world `[x, z]` in f32), `index_range` (`[first_index, count]` into the mesh) |
| `Road` | `osm_id`, `highway`, optional `name`, `width_m`, `points` (world `[x, z]` polyline) |
| `MeshData` | `positions` (f32 x 3), `normals`, `indices` (u32) |

The content type is `application/vnd.observation.bundle`. Postcard is not self-describing, so any change to these types needs a version bump; the decoder rejects unknown versions and bad magic. The server also ignores cached files whose magic or version does not match.

### Tile proxy and OSM policy compliance

The client never talks to the tile server. `GET /api/tiles/{z}/{x}/{y}.png` validates the numbers (zoom at most 19, x and y inside the tile grid), so cache paths are built only from validated integers. Behaviour follows the [OSM tile usage policy](https://operations.osmfoundation.org/policies/tiles/):

- A distinct, identifying `User-Agent` including the operator's contact (`OBS_CONTACT`).
- The client's `Referer` is forwarded upstream.
- Upstream `Cache-Control: max-age` or `Expires` is honoured but never below 7 days.
- Cached tiles are revalidated with `If-None-Match` and `If-Modified-Since`; a 304 only refreshes metadata.
- At most 4 concurrent upstream requests.
- No prefetching: only tiles the browser asks for are fetched, and the client only asks for the tiles it is showing.
- On upstream failure a stale copy is served; with no copy the error is returned. Upstream 429 or 503 becomes a 503 with `Retry-After: 30`.
- Responses must be PNG (magic bytes checked) and at most 2 MiB.
- The disk cache stops accepting new tiles at 512 MiB; there is no eviction.
- Attribution ("© OpenStreetMap contributors", linked to the copyright page) is always visible in the status bar and in the map dialog.

The default upstream is the OSM Foundation's volunteer-run server and is not intended for heavy production use. Operators who expect real traffic should point `OBS_TILE_URL` at a provider they have an agreement with. For Overpass the server follows the public instance's fair-use expectations: one request at a time, an identifying `User-Agent`, bounded query areas, caching, and backoff on 429.

### Demo street

`GET /api/demo` serves a bundle built once at start-up from two committed files in `crates/observation-server/fixtures/`: `demo-bbox.txt` (`47.7075,-122.208,47.7098,-122.204`, about 299 x 256 m of a residential street in Juanita, Kirkland WA) and `demo-overpass.json`. The JSON was retrieved on 2026-10-18 with one request to the OSM API (`/api/0.6/map?bbox=...`) and converted to the Overpass `out geom` shape with `osm_to_overpass.py`. It therefore goes through the same parser as live data. It is © OpenStreetMap contributors (ODbL). `fetch.sh` re-records it from Overpass with a single polite request. The demo is the offline path used by tests and by first-time users.

### Middleware and headers

Layers, outermost first: request tracing, security headers, gzip and brotli compression (on request), a 60 s timeout, a 16 KiB request body limit. Every response carries:

- `Content-Security-Policy: default-src 'self'; script-src 'self' 'wasm-unsafe-eval' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; connect-src 'self'; worker-src 'self' blob:; object-src 'none'; base-uri 'none'; frame-ancestors 'none'`
- `X-Content-Type-Options: nosniff`, `X-Frame-Options: DENY`, `Referrer-Policy: strict-origin-when-cross-origin`

`'unsafe-inline'` for scripts is there because trunk emits an inline initialiser. The server sets no HSTS header; terminate TLS in a reverse proxy and set it there.

## Client

### Structure

The client is a Bevy app (`DefaultPlugins` with WebGL2) whose UI is egui via `bevy_egui`. The canvas is `#observation-canvas`, and egui panels around it set the 3D camera's viewport rectangle, so the scene is never drawn under a panel.

| Plugin | Role |
| --- | --- |
| `NetPlugin` | `fetch` bundles and tiles from the page's origin, reply through a channel drained every frame. Tile queue: at most 6 requests in flight, newest first, soft cap of 400 tiles |
| `WorldPlugin` | Builds the 3D area on load: building mesh, ground, map tiles draped on the ground (at most 64 tiles, plus a 150 m margin around the area), lights, coverage overlay assets |
| `CamerasPlugin` | Camera bodies (a small coloured box with a lens) and view-cone gizmos |
| `CoveragePlugin` | Incremental coverage computation and overlay painting |
| `InputPlugin` | Orbit camera, pointer and touch gestures, tool actions, shortcuts |
| `PersistPlugin` | Autosave, restore, JSON export and import |
| `UiPlugin` | Panels, dialogs, toasts, theme application |

Resources: **`Scene`** is the user's document (area, origin, cameras, analysis parameters, a revision counter that drives sync, coverage and autosave). **`UiState`** is transient state (tool, selection, overlay mode, toggles, toasts). **`Models`** holds the shipped catalogue and the scene's custom models. **`Area`** holds the loaded bundle and the precomputed analysis layers.

### Message flow

```
 UI "Load area" / Demo / restore / import
        | LoadArea { area, keep_cameras }
        v
 NetPlugin: fetch /api/area?bbox=... or /api/demo      (only the latest request counts)
        | decode bundle (magic + version checked)
        v
 AreaLoaded { request, result }
        v
 WorldPlugin: despawn old area entities, build StaticLayers + meshes,
              reset coverage state, set Scene.area / origin, insert Area
        v
 CoveragePlugin: compute per-camera visibility, update counts and stats, repaint
```

A failed load shows an error toast and leaves the previous area in place. Cameras are kept only when `keep_cameras` is set and the area is unchanged (restore and import); otherwise loading an area clears them.

### Camera placement and input

Pointer picking uses Bevy's mesh ray casting against three kinds of target: building mesh, ground, camera bodies. A hit whose normal is mostly horizontal counts as a wall: the camera is placed 0.15 m off the surface along the normal, at the clicked height, facing outward. Any other hit is a pole at the model's default mount height. Presses move into a pending state and become a click, orbit or pan only after travelling 5 px, so taps and drags are distinguished on touch and mouse alike.

## Coverage algorithm

Core code in `observation-core`; the client only schedules it and paints the result.

**Static layers** (built once per area, `StaticLayers`):

- A **ground grid** with 0.5 m cells covering the bounding box (a 500 m square is one million cells).
- An **occluder index** of building prisms.
- A **building mask** (cells whose centre is inside a footprint) and a **street mask** (cells within half a road's width of its polyline, excluding building cells).
- **Façade cells**: every wall split into patches of about 1 m, each with a centre, an outward normal and four corners. Hole walls face into the hole.

**Per camera.**

1. **Effective range.** For a DORI level with `ppm` pixels per metre: `range = width_px / (2 * ppm * tan(hfov / 2))`, capped at 100 m, with `hfov` clamped to 170 degrees (a rectilinear model breaks down towards 180). At night the range is `min(DORI range, IR range)`; with no published IR range 7.5 m is assumed. Only horizontal field of view and width enter the range.
2. **View cone.** A rectangular pyramid defined by the eye, forward/right/up vectors (right is derived from yaw, so it stays stable at +/-90 degrees of pitch; roll rotates it about the view axis) and the tangents of the half angles. A point is inside if it is in front of the eye, within `range` in straight-line distance, and `|d . right| <= z tan(hfov/2)` and `|d . up| <= z tan(vfov/2)` where `z = d . forward`.
3. **Ground samples.** Candidate cells are those in the cone's square bounds, minus building cells. Each cell centre is lifted to the subject height (default 1.0 m). It counts if it is inside the cone and the segment from the eye to it is not blocked.
4. **Façade samples.** Each patch that faces the eye and lies within range is tested at its centre pushed 5 cm out along the normal, with the same cone and line-of-sight tests.

**Line of sight.** Buildings are vertical prisms (footprint, `min_height` to `height`) with flat roofs, held in a **uniform 2D grid of 8 m cells**. A segment is walked through the grid with the **Amanatides-Woo** traversal, visiting only the cells it touches. For each candidate prism the test is exact: collect the parameters where the segment's plan-view projection crosses a footprint edge, sort them, and for each interval check whether its midpoint is inside the footprint (even-odd rule over outer and hole rings) and whether the segment's height range over that interval overlaps the prism's height range. Because the height varies linearly along the segment and the prism is a vertical extrusion, this is an exact segment-prism intersection, not a sampled one.

**Counting and statistics.** Each camera's result is a sorted list of ground and façade cell indices. A shared per-cell `u8` counter is incremented (saturating) for every enabled camera that sees the cell. Statistics are computed from the counters: street area, street area seen by at least one and by at least two cameras, ground seen outside buildings (street included), and façade area (patch areas, so the figure is by area, not patch count) total and seen.

**Incremental updates.** The client caches each camera's visibility under a 64-bit hash of everything that affects it: position, yaw, pitch, roll, the model's fields, and the analysis parameters (level, lighting, subject height). When the scene changes, only cameras whose key changed are recomputed; their old result is subtracted from the counters and the new one added. Disabled or deleted cameras are subtracted. Loading a new area resets everything.

**Painting.** Counters become colours (overlap ramp for 1, 2, 3, 4+ cameras, or per-camera colours) and are written to an RGBA texture with one texel per ground cell, drawn unlit on a quad just above the ground, and to per-vertex colours of one quad per façade patch, offset 3 cm from the wall. Walls can be hidden without recomputing.

### Why CPU ray casting rather than GPU shadow maps

The usual way to show "what a light or camera sees" is to render a depth map from the camera and compare. It was rejected for these reasons:

- **Exactness.** With flat-roofed prisms the CPU test is an exact geometric answer. Shadow maps need depth bias, which causes light leaks and self-shadowing at wall bases, and resolution-dependent edges that would be hard to explain next to a "percent seen" figure.
- **One source of truth.** The statistics and the overlay come from the same results, so the numbers always agree with what is drawn. With shadow maps the statistics would need a GPU read-back or a second, different calculation.
- **WebGL2 safety.** The client targets WebGL2. Rendering to many depth targets and reading them back is awkward and slow there, and cube or array textures per camera hit device limits.
- **No per-camera cap.** The cost per camera is only the cells it covers; there is no texture array to size, and up to 256 cameras are allowed in a scene.
- **Testable.** Occlusion and coverage live in `observation-core` and are unit-tested natively with no GPU. The 3D view does use a shadow-mapped sun light, but only for appearance; it plays no part in coverage.

The cost is CPU time. A camera's recompute is synchronous on the main thread and scales with the cells inside its range (at most about 160,000 ground cells for a 100 m range, each needing a ray cast only if it is inside the cone) plus a pass over all façade patches. Caching keeps this to the camera being edited, but dragging a camera recomputes it on every frame. Moving the work to a Web Worker is the obvious next step.

## Design tokens and themes

`theme.rs` defines a `Tokens` resource: the only place colours, spacing (4, 8, 12, 16, 24 px), radii (4, 8, 12), the type scale (12, 14, 16, 20 px) and layout sizes (32 px hit target, 64 px tool rail, 320 px inspector, 48 px top bar, 28 px status bar) are written down. It has a light and a dark variant, chosen from `prefers-color-scheme` once at start-up, and `apply` pushes it into egui's style. World colours (sky, ground, buildings, selection, aim marker), the eight camera colours and the four-step overlap ramp are tokens too.

`web/index.html` mirrors the handful of values that the loading screen and the small-screen gate need as CSS custom properties, with a dark variant under `@media (prefers-color-scheme: dark)`; the file says to keep it in sync with `theme.rs`.

`scripts/check-design-tokens.sh` runs in CI. It scans every `.rs` file under the client's `src/` except `theme.rs` and fails on colour constructors (`Color32::from_rgb`, `Color::srgb(...)`, `Srgba::new(...)`, named `Color32::` constants and similar) or hex colour strings, so colours cannot drift away from the tokens.

## Viewport gate

The minimum supported viewport is 768 x 600 CSS px, enforced twice: a CSS media query in `index.html` (`max-width: 767.98px` or `max-height: 599.98px`) shows a static notice and hides the loader and canvas before the application is needed, and an in-app gate (`MIN_VIEWPORT` in `theme.rs`) replaces the UI with the same message if the window shrinks below the minimum at run time. The media query only hides the page and shows the notice; it does not stop the WebAssembly module from loading. Its value is that a phone user sees the explanation at once instead of a half-working UI.

## Persistence

The scene is serialised with `SceneFile` (`format: "observation-scene"`, `version: 1`) to `localStorage` under `observation.scene.v1` whenever its JSON changes, checked every 1.5 s. At start-up the stored scene is restored (and its area re-fetched); a missing entry loads the demo and opens the help card; an unreadable entry shows an error and falls back to the demo. Export and import use the same format through a file download and a file picker. `SceneFile::from_json` is the single validation point (see the threat model). Camera positions are relative to the stored origin, which comes from the bundle, so a saved scene lines up with its buildings as long as the area is regenerated from the same box.

## Security and supply chain

### Workflows

All five workflows set `permissions: {}` at the top and grant each job only what it needs. Every action is pinned to a full commit SHA, and checkouts use `persist-credentials: false`.

| Workflow | Triggers | What it does |
| --- | --- | --- |
| `ci.yml` | every push and pull request | `cargo fmt --check`; clippy with `-D warnings` for the native workspace and, separately, the client for `wasm32-unknown-unknown`; tests; the design-token guard; a release `trunk build` with a size report and `dist/` artifact; a Docker build with a smoke test (image runs as 65532, `/healthz` answers, `dist/index.html` is present) |
| `security.yml` | push to `main`, pull requests, daily, manual | `cargo-deny` (advisories, bans, licences, sources); `cargo-audit`, which on the scheduled run opens issues for new advisories; Trivy filesystem scan (vulnerabilities, Dockerfile misconfiguration, secrets) and image scan, failing on fixable critical or high findings and uploading SARIF |
| `codeql.yml` | push and pull request to `main`, weekly | CodeQL for `rust` and for the workflows themselves (`actions`) |
| `scorecard.yml` | push to `main`, weekly, branch-protection changes | OpenSSF Scorecard, results published and uploaded to code scanning |
| `release.yml` | push to `main`, tags `v*` | Builds and pushes `ghcr.io/panevain/observation` with tags `edge`, the full commit SHA, and semver tags; SLSA provenance (`mode=max`), an SBOM, and a GitHub build-provenance attestation |

**Dependabot** proposes weekly updates for Cargo, GitHub Actions and Docker, grouped by minor and patch, after a 7-day cooldown that gives the community time to catch a bad release (security updates are not delayed). Dependabot alerts and security updates are repository settings, not part of the config file.

**Trivy.** The Trivy binary is downloaded from a pinned GitHub release and verified against a pinned SHA-256 instead of using `aquasecurity/trivy-action`, whose tags were compromised in March 2026 (CVE-2026-33634). Trunk 0.21.14 is installed the same way: a pinned release tarball verified against its SHA-256 in CI (x86_64) and in the Dockerfile (amd64 and arm64).

**cargo-deny** (`deny.toml`): yanked crates and advisories are errors with no ignores; only an allow-list of permissive licences passes; wildcard versions are denied (workspace path dependencies excepted); duplicate versions only warn, since Bevy's tree has many; crates must come from crates.io and git dependencies are denied. The graph is resolved for x86_64 and aarch64 Linux and wasm32.

**Container image.** Multi-stage. The builder is `rust:1.97.1-slim-bookworm`, pinned by digest to match `rust-toolchain.toml`, and builds with `--locked`. The runtime is `gcr.io/distroless/cc-debian12:nonroot`, also pinned by digest, which has no shell and runs as uid and gid 65532. Release images can be verified with `gh attestation verify`; see [SECURITY.md](../SECURITY.md) for reporting and scope.

### Threat model

There are no accounts, sessions or personal data. The assets worth protecting are the server's availability, its standing with the upstream services, and the user's browser.

| Threat | Mitigations | Residual risk |
| --- | --- | --- |
| **Untrusted scene files** (imported JSON, or tampered `localStorage`) | Parsed with serde into typed structures; `format` and `version` checked; at most 256 cameras and 64 custom models; positions and angles must be finite and below 10,000; subject height 0-10 m; custom models validated (kebab-case id, field-of-view, resolution and IR ranges); labels are drawn as text, never as markup; a failed import changes nothing | No limit on the file's byte size; duplicate camera ids and unknown `model_id`s are not rejected (an unknown model just shows as "Unknown model" and is skipped); a stored bounding box is not range-checked on import, but the server validates it when the area is fetched |
| **Untrusted bounding-box input** (`/api/area`) | Strict parsing; ranges, ordering and 20-500 m size enforced server-side regardless of the UI; request body limit; coordinates are only ever formatted back as fixed-precision numbers into the query, so there is no injection into Overpass QL; cache file names come from a sanitised key | Distinct boxes make distinct cache entries, and bundles are never evicted or size-capped, so a client that sweeps many boxes can grow the cache directory |
| **Abuse of the proxy** (tile and Overpass amplification) | Upstream URLs are fixed by operator configuration, never by request data; tile coordinates validated, so no path traversal or arbitrary URLs; single-flight and caching for bundles; one Overpass request at a time with a cooldown after 429 or 504; at most 4 upstream tile fetches; 512 MiB tile cap; PNG magic and size checks; 60 s timeout; responses cached by browsers for a day or a week | No per-client rate limiting. A flood of distinct boxes is cut off by the 3-deep Overpass queue (503s), but one client can still occupy that queue. Once the tile cache is full every uncached tile request goes upstream (still limited to 4 at a time). The 32 MiB Overpass cap relies on `Content-Length`. Run it behind a reverse proxy that rate-limits per client |
| **Browser-side injection** | Strict CSP (self-only connections, no framing, no objects, no `base`), `nosniff`, `X-Frame-Options: DENY`; the UI is a canvas and draws all text through egui | CSP allows inline scripts and styles because of trunk's output; no HSTS from the app itself |
| **Supply chain** | Everything above: pinned SHAs and digests, checksum-verified tools, `--locked` builds, cargo-deny, Trivy, CodeQL, Scorecard, provenance and SBOM | Dependencies still update through Dependabot PRs, so review them |
| **Data poisoning** (bad OSM data) | Bundles are rebuilt from a fresh parse; heights are clamped; degenerate geometry is dropped | OSM edits can change results; cached bundles live for the TTL (7 days) and can be served stale if upstream is down |

## Testing

- **Core unit tests** cover projection round trips and axes, bounding-box parsing and snapping, tile maths, height and length parsing, extrusion winding and normals (with holes and raised parts), bundle round trips and rejection of bad input, road widths, DORI ranges, yaw and pitch conversion, catalogue parsing, view cones, exact occlusion cases, coverage on a small synthetic street, and scene file validation. An integration test (`crates/observation-core/tests/catalog.rs`) parses the shipped catalogue and requires a manufacturer `https://` source for every model that has a vendor.
- **Server unit tests** cover configuration parsing, cache key safety and TTL, Overpass parsing and ring stitching, tile path validation and the demo bundle decoding to a plausible number of buildings.
- **Server integration tests** (`crates/observation-server/tests/api.rs`) run the real router against **mock Overpass and tile servers** on loopback: security headers, `/api/config`, bad boxes, compression, fetch-once-then-cache, single-flight under concurrency, 503 and fast failure during the cooldown, the single retry on 504, 502 responses not being cached, serving expired bundles when upstream fails, tile proxying, tile validation without upstream calls, revalidation and stale-on-error for tiles, rejection of non-PNG tiles, SPA fallback with cache headers, and JSON 404s for unknown API paths.
- **CI static checks**: formatting, clippy with warnings as errors on native and wasm targets, and the design-token guard.
- **Build checks**: the release `trunk build` and the Docker build with a smoke test.

The client has no unit tests yet. The CI step that would run them only runs if the client has a library target, which it currently does not, so client behaviour is covered by compilation, clippy and the core tests of the logic it uses.

## Known limitations and future work

- Flat-roofed boxes only: no pitched roofs, no building parts, no terrain, no vegetation, fences or vehicles. Roof geometry would need a different occlusion test.
- Coverage is computed on the main thread. A Web Worker, or a coarser first pass while dragging, would remove frame hitches for long-range cameras.
- The bundle cache has no size limit or eviction, and the tile cache stops caching at its cap instead of evicting.
- Per-client rate limiting belongs in the server (or a proxy in front of it).
- Custom models cannot be edited or deleted in the app; there is no undo; there is a single autosave slot.
- The theme follows the system setting at load only. The map dialog and the 3D scene do not respond to a later change.
- Night modelling uses IR range only; it does not account for floodlights, street lighting or shutter behaviour.
- The client hard-codes its API paths and the 20-500 m limits rather than reading them from `/api/config`.
