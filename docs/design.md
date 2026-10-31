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
| `observation-core` | Pure logic with no engine dependency, shared by both sides. `geo` (projection, bounding boxes), `regions` (region status JSON), `tiles` (slippy-map maths), `building` (height inference, extrusion), `bundle` (wire format, road widths), `camera` (models, DORI, catalogue parsing), `frustum` (view cones), `occlusion` (line of sight), `coverage` (raster, statistics), `scene` (saved files) |
| `observation-server` | `config` (environment), `overpass` (client and parser), `bundles` (cache plus single-flight), `cache` (disk), `tiles` (proxy), `demo` (fixture bundle), `regions` (region agent client and status relay), `routes/*`, `error` |
| `observation-regions` | Optional region agent binary: `catalog` (Geofabrik index), `poly` (boundaries, containment), `import` (download, filter, Overpass import), `registry` (regions, queue, persistence), HTTP API in `lib` |
| `observation-client` | Bevy app compiled to `wasm32-unknown-unknown`: networking, 3D world, camera bodies, coverage systems, input, persistence, egui UI, design tokens |

Core, server and region agent are the workspace's default members, so plain `cargo build` and `cargo test` do not compile the Bevy client. Workspace lints forbid `unsafe` code and warn on `unwrap`, `dbg!` and `todo!`; CI turns warnings into errors.

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
| `GET /api/regions` | [Local regions](#local-regions) status: `{agent_online, manageable, snapshot}`, `no-store`. 404 when no region agent is configured (as for every `/api/regions` route) |
| `GET /api/regions/events` | The same status as server-sent events (`event: regions`), once on connect and after every change. Not subject to the request timeout; at most 256 streams at once (503 beyond); streams end when the server shuts down |
| `GET /api/regions/catalog` | Importable Geofabrik extracts, `[{id, name, parent}]` |
| `POST /api/regions/{import,cancel,delete}` | `{"id": "<extract id>"}` with `Authorization: Bearer <OBS_ADMIN_TOKEN>`; answers the new status. 403 when `OBS_ADMIN_TOKEN` is unset, 401 for a missing or wrong token, 409 when an import of that region is already queued or running |
| `/api/*` (anything else) | JSON 404, deliberately not the SPA page |
| everything else | Static client from `OBS_STATIC_DIR`, with `index.html` as fallback. HTML is `no-cache`; other files are treated as content-hashed (trunk `filehash = true`) and get `public, max-age=31536000, immutable` |

Errors are JSON `{"error": "..."}`: 400 bad request, 401 and 403 (region management), 404, 409 (region import already running), 502 upstream failure, 504 upstream timeout, 503 rate limited (with `Retry-After`), 500 internal (detail logged, never sent), 408 on request timeout.

### Configuration

Read from the environment at start-up; empty values count as unset; invalid `OBS_BIND`, `OBS_BUNDLE_TTL_DAYS`, `OBS_OVERPASS_URL`, `OBS_TILE_URL`, `OBS_METRICS_BIND` (or one equal to `OBS_BIND`) or `OBS_LOG_FORMAT`, and an address that cannot be bound, stop the server with an error.

| Variable | Default | Meaning |
| --- | --- | --- |
| `OBS_BIND` | `0.0.0.0:8080` | Listen address |
| `OBS_STATIC_DIR` | `dist` | Built client directory (relative to the working directory). A warning is logged if it has no `index.html` |
| `OBS_CACHE_DIR` | `cache` | Root of the bundle and tile caches |
| `OBS_CONTACT` | `https://github.com/panevain/observation` | Put in the `User-Agent` (`observation/<version> (+<contact>)`); set your own |
| `OBS_OVERPASS_URL` | `https://overpass-api.de/api/interpreter` | Overpass interpreter, or up to 4 comma-separated http(s) interpreters tried in order (for example your own instance, then a public one). Malformed entries, and literal loopback, private or link-local addresses (unless `OBS_ALLOW_PRIVATE_UPSTREAMS`), stop the server at startup |
| `OBS_TILE_URL` | `https://tile.openstreetmap.org/{z}/{x}/{y}.png` | Upstream tile template (`{z}`, `{x}`, `{y}`; `{s}` becomes `a`). Same URL checks as `OBS_OVERPASS_URL`; `{x}`, `{y}` and `{z}` may not appear in the host |
| `OBS_ALLOW_PRIVATE_UPSTREAMS` | unset | `1` lets `OBS_OVERPASS_URL` and `OBS_TILE_URL` point at loopback, private or link-local addresses (for example your own Overpass instance on a private network). Without it, those addresses are refused at startup, and a host name that resolves to one is refused when connecting. Upstream redirects are never followed, and `HTTP_PROXY`/`HTTPS_PROXY` are ignored unless this is `1` (a proxy would resolve the upstream's name itself). `OBS_REGIONS_URL` is exempt: the agent is private by design |
| `OBS_TILE_ATTRIBUTION` | `© OpenStreetMap contributors` | Attribution embedded in bundles and `/api/config` |
| `OBS_BUNDLE_TTL_DAYS` | `7` | Bundle freshness; fractional values allowed, `0` revalidates every time |
| `OBS_LOG_FORMAT` | `text` | `json` writes one JSON object per log event; set the level with `RUST_LOG` (default `info`) |
| `OBS_REGIONS_URL` | unset | Base URL of the [region agent](#local-regions), such as `http://regions:8090`. Unset turns local regions off (and hides them in the UI) |
| `OBS_REGIONS_TOKEN` | unset | Bearer token sent to the region agent; set the same value on the agent |
| `OBS_ADMIN_TOKEN` | unset | Token the UI must send to import, update, cancel or delete regions. Unset makes regions read-only. Use a long random value (`openssl rand -hex 32`): it is the only guard on a feature that downloads gigabytes |
| `OBS_METRICS_BIND` | unset | Address for a separate Prometheus listener such as `127.0.0.1:9090`. Unset disables metrics. It is not the public port, so `/metrics` is never reachable through the app's own address |
| `RUST_LOG` | `info` | `tracing` filter |

### Observability

Every response carries an `x-request-id` header (taken from the request if the client or a proxy sent one, otherwise a UUID). Request tracing stays at `DEBUG`, so no access log is written by default (see the privacy table); the id is on the request span, which is `DEBUG` under the `tower_http` target (`RUST_LOG=tower_http=debug`), and upstream warnings from a bundle fetch carry the id of the request that started it. With `OBS_METRICS_BIND` set, `GET /metrics` on that address serves Prometheus text:

| Metric | Labels | Meaning |
| --- | --- | --- |
| `observation_http_requests_total` | `method`, `route`, `status` | Requests by matched route pattern, never the raw path; methods outside the standard set are labelled `other`. Timed-out requests (408) are included, and requests the client abandons are counted with `status="cancelled"`. The request counter and histogram exist only when `OBS_METRICS_BIND` is set |
| `observation_http_request_duration_seconds` | `method`, `route` | Latency histogram (buckets from 5 ms to 80 s) |
| `observation_bundle_cache_total` | `result`: `hit`, `built`, `stale_served`, `error` | `hit` is per request served from a fresh file. The others are per upstream refresh, which concurrent requests for the same area share |
| `observation_tile_cache_total` | `result`: `hit`, `miss` (nothing cached), `refreshed` (expired copy replaced), `revalidated` (304), `stale_served`, `error` (upstream failed, no copy) | Tile cache outcomes, one per request |
| `observation_overpass_fetches_total` | `outcome`: `ok`, `error`, `rate_limited` | Upstream Overpass fetches |
| `observation_region_queries_total` | `outcome`: `ok`, `not_covered`, `error` | Bundle builds tried against a local region; the last two fall back to Overpass |

Fixed in code (not configurable): tile minimum freshness 7 days, tile cache cap 512 MiB, at most 4 concurrent upstream tile fetches, Overpass retry pause 2 s and cooldown 30 s, Overpass timeout 35 s per attempt, tile client timeout 20 s, whole-request timeout 80 s, request body limit 16 KiB. The container sets `OBS_BIND=0.0.0.0:8080`, `OBS_STATIC_DIR=/app/dist` and `OBS_CACHE_DIR=/var/cache/observation`. The cache directory is not a volume; mount one there (writable by uid 65532) to keep it across container runs.

### Bundle pipeline

For `GET /api/area`:

1. **Validate.** `bbox` must be four finite numbers in Overpass order; latitudes within +/-85, longitudes within +/-180; south < north and west < east. Each side, measured at the centre latitude, must be 20-500 m. Anything else is a 400. (Boxes crossing the antimeridian are not supported.)
2. **Snap.** The box is expanded outward to a 0.0001 degree grid (about 10 m) so nearby requests share a cache entry. The cache key is `south_west_north_east` with five decimals. The bundle's origin is the snapped box's centre, and snapping is deterministic, so a saved scene that stores the requested box reproduces the same frame.
3. **Cache.** `bundles/<key>.obsb` on disk; fresh while its modification time is within the TTL and, when a ready local region's bounds hold the box, later than that region's import (so importing or refreshing a region rebuilds the bundles in it on their next request).
4. **Single-flight.** Concurrent requests for one key share a single in-flight job. The job runs as its own task, so it finishes and fills the cache even if every waiting client disconnects.
5. **Fetch.** From a [local region](#local-regions) when one is ready and its bounds hold the box: the agent runs the query below against the region's own database, and anything but a result within 15 s (no region holds the area exactly, the agent is down, busy or fails) falls back to public Overpass. The time spent on the local try counts against the Overpass retry budget below. Otherwise one Overpass query per box (below). Only one Overpass request is in flight at a time (a semaphore with one permit), and at most 3 more may wait for it; further misses fail fast with 503 and `Retry-After: 10`. A transient failure (connection error, no answer within 35 s, 429, 502, 503 or 504, or a `remark` saying the query timed out) is retried after a 2 s pause: on the next configured interpreter, or once on the same one when only one is configured. An interpreter that answers 429 or 504 (on its last try in that fetch) rests for a 30 s **cooldown** and is skipped meanwhile; while every interpreter rests, new fetches fail immediately with 503 and `Retry-After`. Retries stop when 90% of the 80 s request timeout has passed (queueing included), so the client always gets an answer rather than a 408. The reply after the last failed attempt is 504 if it timed out, 503 if it was refused as busy, and 502 otherwise. Other statuses, a `remark` reporting any other runtime error (such as running out of memory) and an unparseable body are 502 without a retry. A response larger than 32 MiB is refused; the cap is enforced while the body streams in, so it does not depend on `Content-Length`.
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
  node["addr:housenumber"](s,w,n,e);
  node["addr:housename"](s,w,n,e);
);
out geom qt;
```

**Parsing.** Closed `building` ways with at least four points and no missing nodes become buildings. Ways that are `outer` members of a building relation are skipped, because the relation covers them. Multipolygon relations have their outer and inner member ways **stitched** end to end (reversing where needed; shared nodes have identical coordinates, so exact comparison is correct) into closed rings. Unclosed fragments are dropped. Each outer ring becomes a building, and each inner ring becomes a hole of the outer ring that contains its first point. A relation's buildings carry the negated relation id so they cannot clash with way ids. `highway` ways become roads when the type has a known width; `area=yes` ways are skipped. Buildings keep their address (`addr:housenumber` or `addr:housename`, with `addr:street` or else `addr:place`, and `addr:unit`; each value trimmed and cut to 80 characters), and nodes with a house number or house name become **address points**. An address with neither `addr:street` nor `addr:place` (common where the street is on an `associatedStreet` relation, which the query does not fetch) takes the name of the nearest road within 100 m, measured from the building's bounding-box centre or the node, as Nominatim does; motorways, `*_link` roads and footways other than `pedestrian` are not used. Otherwise two houses tagged only `12` would share a label, and so a group. Each address point records the building it lies in (inside the outer ring and outside holes, or else on the outline within 0.5 m, as an entrance node is).

**Height inference** (`building::infer_heights`), in priority order:

1. `height` (a length such as `12`, `12 m`, `40 ft`, `12'6"`), if above 0.5 m.
2. `building:levels` x 3 m per storey, with `roof:levels` added to the level count; when no roof levels are tagged, 1 m is added for the roof.
3. A default by `building=*` value: 3 m for shed, garage(s), carport, hut, roof, kiosk, cabin; 4 m for bungalow and static caravan; 6.5 m for house, detached, semidetached house, terrace, residential, farm; 10 m for apartments, dormitory, hotel; 12 m for church and cathedral; 6 m otherwise.

The result is clamped to 1-300 m. `min_height`, or `building:min_level` x 3 m, gives the base of building parts (kept at least 0.5 m below the top).

**Road widths** (`bundle::road_width_m`): a `width` tag between 1 and 60 m wins; else `lanes` (1-11) x 3.25 m; else a default by type: motorway and trunk 14 m, primary 10, secondary 8, tertiary 7, residential and unclassified 6, living street 5, service 4, footway, path, pedestrian, cycleway and steps 2, `*_link` ramps 6. Other highway types are ignored.

**Extrusion.** Each footprint becomes vertical walls with outward normals (separate vertices per wall face, so shading stays flat) and a flat roof triangulated with `earcut`, holes included. Raised parts also get an underside. Footprints that are degenerate (fewer than three distinct points, near-zero area, or no height) are dropped. Roofs are always flat: this is what makes the occlusion test exact (see below).

### Local regions

Public Overpass instances are shared and often slow or busy. An operator can run the **region agent** (`crates/observation-regions`, the `regions` image target) next to the server, and import whole [Geofabrik](https://download.geofabrik.de) extracts (a city, state or country) into it from the UI. Areas inside an imported region are then answered from the server's own copy in tens of milliseconds, and public Overpass is used only elsewhere.

```
browser ──SSE /api/regions/events──▶ observation-server ──long-poll GET /regions──▶ observation-regions
        ──POST /api/regions/import──▶ (admin token)     ──POST /regions/import───▶  ├─ download .osm.pbf + .poly
                                                                                     ├─ osmium tags-filter
area bundle miss ──────────────────▶ bundles.rs ────────POST /interpreter?bbox=──▶  ├─ osmium cat | update_database
                                       │ 404 / error                                 └─ osm3s_query --db-dir=<region>
                                       └──▶ public Overpass (as before)
```

**The agent** runs in a Debian image with Overpass API 0.7.62 (built from the sha256-pinned release tarball) and `osmium-tool`. It keeps one Overpass database per region under its data volume and imports one region at a time from a queue:

1. **Download** the extract's `-latest.osm.pbf` from the URL in Geofabrik's `index-v1-nogeom.json` (ids are only accepted if the index lists them, and downloads must be on the index's own host), refusing anything over `OBS_REGIONS_MAX_DOWNLOAD_MB`, and its `.poly` boundary.
2. **Filter** with `osmium tags-filter w/building r/building w/highway n/addr:housenumber n/addr:housename` (referenced nodes and member ways are kept). Regions imported before address nodes were kept have building addresses only, until their next refresh. This cuts the data to roughly a tenth.
3. **Import** with `osmium cat -f osm | update_database --meta=no` (lz4 compression) into a new directory under `work/`. Progress counts the `<node`, `<way` and `<relation` elements streamed against `osmium fileinfo`'s totals; download progress is bytes against `Content-Length`.
4. **Swap.** The finished database moves to `db/<region>-<time>/` with its boundary, and replaces the old one in `regions.json`. Queries already running on the old database finish on it; its directory is removed when the last one ends.

Plan disk space from the database, not the download: an Overpass database is far larger than the filtered extract (Monaco's 0.7 MB extract became 57 MB, Liechtenstein's 3.3 MB became 156 MB), and an update needs room for the old and the new database at once.

A cancelled, failed or interrupted import leaves the previous data in service and its work directory is removed (an import that was running when the agent stopped is queued again on start). Each queued import has its own id, so a cancelled task that is still winding down cannot touch a newer import of the same region. `regions.json` is written with fsync and rename while the registry is locked; if it exists but cannot be read, the agent refuses to start rather than delete the databases it can no longer account for. Queries run `osm3s_query --db-dir` directly (no dispatcher, since a database is never written after its import), at most 4 at a time (`OBS_REGIONS_QUERY_CONCURRENCY`) with up to 32 waiting, 30 s each, results capped at 32 MiB (a larger result stops the query at once). A query is answered only when the requested box lies wholly inside a region's boundary polygon (regions are first narrowed by their bounds) (corners inside, no boundary edge or hole reaching into it); near a border the agent answers 404 and the server uses public Overpass, because an extract is complete only inside its boundary.

**Freshness and invalidation.** Regions are re-imported automatically after `OBS_REGIONS_REFRESH_DAYS` (default 7; Geofabrik publishes daily), or on demand with the region's update button. A scheduled refresh that fails or is cancelled is tried again a day later, not at the next hourly check. Each import records the extract's OSM replication timestamp. The server keeps a copy of the agent's region list (long-polling `GET /regions?since=<version>`, which answers on the next change or after 25 s) and treats cached bundles older than the newest covering region's import as stale, so the next request for such a box rebuilds it from local data. Browsers may still use their own copy of a bundle for up to a day (`max-age=86400`).

**Progress in the browser.** The server relays the agent's list to the browser as server-sent events. The client opens one `EventSource` at start-up (only when `/api/config` says `regions: true`), shows the running import in the top bar, and toasts when an import finishes or fails. The **Local map data** dialog lists regions with phase, progress and detail, and imports from a searchable list of extracts. Nothing in the page waits for an import.

**Security.** The agent has no published port in `compose.yaml` and checks `OBS_REGIONS_TOKEN` on every route but `/healthz`. Management through the server needs `OBS_ADMIN_TOKEN` (compared in constant time); the UI keeps it in `sessionStorage` for the browser session only. The agent never takes a URL from a request: region ids are validated (`[a-z0-9-]` parts joined by `/`) and looked up in the index, and child processes get fixed arguments.

**Agent configuration** (environment of the `regions` container):

| Variable | Default | Meaning |
| --- | --- | --- |
| `OBS_REGIONS_BIND` | `0.0.0.0:8090` | Listen address; keep it on a private network |
| `OBS_REGIONS_DATA_DIR` | `/data` | Databases, downloads, `regions.json` and the cached index. Mount a volume; a country takes several GB during its import |
| `OBS_REGIONS_TOKEN` | unset | Required bearer token (the agent warns at start-up without one) |
| `OBS_REGIONS_MAX_DOWNLOAD_MB` | `2048` | Largest extract downloaded |
| `OBS_REGIONS_REFRESH_DAYS` | `7` | Re-import ready regions after this many days; `0` never |
| `OBS_REGIONS_QUERY_CONCURRENCY` | `4` | `osm3s_query` processes at once (1-64) |
| `OBS_REGIONS_CATALOG_URL` | Geofabrik's `index-v1-nogeom.json` | Extract index; downloads must share its host, so a mirror must serve both |
| `OBS_ALLOW_PRIVATE_UPSTREAMS` | `0` | `1` lets the index and downloads use loopback, private or link-local addresses (a mirror on your network); otherwise refused, and redirects never leave the index's host; proxy variables are ignored unless `1` |
| `OBS_REGIONS_OVERPASS_BIN`, `OBS_REGIONS_OSMIUM` | `/opt/overpass/bin`, `osmium` | Tool locations (set in the image) |
| `OBS_CONTACT`, `OBS_LOG_FORMAT`, `RUST_LOG` | | As for the server; `OBS_CONTACT` goes into the agent's `User-Agent` to Geofabrik |

### Bundle format

A response body is:

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 4 bytes | Magic `OBSB` |
| 4 | 2 bytes | Format version, `u16` little-endian (currently `2`; version 2 added addresses) |
| 6 | rest | `AreaBundle`, encoded with [postcard](https://docs.rs/postcard) |

The client adds `v=<format version>` to bundle URLs (the server ignores it), so a bundle the browser cached in an older format is never handed to a newer client.

`AreaBundle` fields, in order: `bbox` (south, west, north, east as f64), `origin` (lat, lon), `buildings`, `roads`, `mesh`, `addresses`, `attribution`, `generated_unix`.

| Type | Fields |
| --- | --- |
| `Building` | `osm_id` (i64; negative for relations), `footprint` (outer ring, holes, `min_height_m`, `height_m`; world `[x, z]` in f32), `index_range` (`[first_index, count]` into the mesh), optional `address` |
| `Address` | Optional `housenumber`, `street` (or `addr:place`), `housename`, `unit` strings |
| `AddressPoint` | `address`, `position` (world `[x, z]`), optional `building` (index into `buildings` of the building it lies in) |
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
- Responses must be PNG (magic bytes checked) and at most 2 MiB (enforced while streaming, with or without `Content-Length`).
- The disk cache is capped at 512 MiB (tile images and their `.json` sidecars). A write that would exceed the cap first deletes the least recently written or revalidated tiles (oldest `.png` mtime, with their sidecars; a 304 touches the `.png`) down to 90 % of the cap, so the directory is not scanned on every write. Each eviction walks the directory, which also corrects the size estimate and removes sidecars without an image. One write evicts at a time; writes arriving meanwhile go ahead, so the cache can briefly exceed the cap. A tile larger than the cap is not cached.
- Attribution ("© OpenStreetMap contributors", linked to the copyright page) is always visible in the status bar and in the map dialog.

The default upstream is the OSM Foundation's volunteer-run server and is not intended for heavy production use. Operators who expect real traffic should point `OBS_TILE_URL` at a provider they have an agreement with. For Overpass the server follows the public instance's fair-use expectations: one request at a time, an identifying `User-Agent`, bounded query areas, caching, and backoff on 429.

### Demo street

`GET /api/demo` serves a bundle built once at start-up from two committed files in `crates/observation-server/fixtures/`: `demo-bbox.txt` (`47.7075,-122.208,47.7098,-122.204`, about 299 x 256 m of a residential street in Juanita, Kirkland WA) and `demo-overpass.json`. The JSON was retrieved on 2026-10-18 with one request to the OSM API (`/api/0.6/map?bbox=...`) and converted to the Overpass `out geom` shape with `osm_to_overpass.py`. It therefore goes through the same parser as live data. It is © OpenStreetMap contributors (ODbL). `fetch.sh` re-records it from Overpass with a single polite request. The demo is the offline path used by tests and by first-time users.

### Middleware and headers

Layers, outermost first: request tracing, security headers, gzip and brotli compression (on request), an 80 s timeout (two Overpass attempts and the pause between them), a 16 KiB request body limit. Every response carries:

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
| `AddressesPlugin` | Each camera's address (`address::address_at` in core: the nearest addressed building or address point within 25 m, measuring a point inside a building to that building), recomputed only for cameras that moved and for a new area; the camera list groups by it unless the camera has its own `group` |
| `CoveragePlugin` | Incremental coverage computation and overlay painting |
| `InputPlugin` | Orbit camera, pointer and touch gestures, tool actions, shortcuts |
| `PersistPlugin` | Autosave, restore, JSON export and import |
| `UiPlugin` | Panels, dialogs, toasts, theme application and following the system light/dark setting |

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

### Bundle size

[![Treemap of the shipped WebAssembly module, one rectangle per crate](images/wasm-bundle-treemap.svg)](images/wasm-bundle-treemap.svg)

The shipped module is about 26.1 MB raw and 7.8 MB gzip. Bevy is half of it (about 12.5 MiB), and `bevy_ecs` alone is 6.8 MiB (27%) because every system function is monomorphised into its own `FunctionSystem<fn(...)>` (2.9 MiB in total), plus reflection and command-queue glue. The same machinery is instantiated for every system of every Bevy plugin, so `bevy_ecs` here is mostly the cost of the plugins' systems rather than the ECS core. Data segments are the next block (4.4 MiB, 18%): font tables, WGSL shader sources, and type-registry strings and vtables, none of which can be attributed to a crate by name. The application's own code (`observation_client` and `observation_core`) is under 1%. The client already builds Bevy with `default-features = false` and a short feature list, and drops `ktx2`, `zstd_rust` and `tonemapping_luts` (see `crates/observation-client/Cargo.toml`), so the remaining levers are structural: trimming further Bevy features such as `bevy_gizmos` or `bevy_picking` if they can be replaced by a few hundred lines of app code, compiling the heavy non-hot crates (`naga`, `wgsl_parse`, `bevy_reflect`) with `opt-level = "z"` through a profile override, and serving the module with brotli instead of gzip. Each of these needs measuring before it is adopted, since attribution is by symbol name after inlining and does not equal the saving from removing a crate.

The figure is measured on the `wasm-release` profile after `wasm-bindgen` and `wasm-opt -Os`, with symbol names kept (`strip = false`, `--debuginfo`) and then excluded from the sizes; the result is within 0.03% of the module `trunk build` ships. The Treemap workflow regenerates it for every release tag and opens a PR. To regenerate it by hand after a dependency or feature change, run `mise run treemap` or `scripts/wasm-size-report.sh` (a fat-LTO build, 15 to 20 minutes on four cores; `--no-build` reuses the last build). It needs only `cargo`, `python3`, and the `wasm-bindgen` and `wasm-opt` binaries that trunk caches, prints a top-15 table, and rewrites `docs/images/wasm-bundle-treemap.svg` through `scripts/wasm-size-treemap.py`, which is stdlib-only and also accepts `twiggy top --format csv` output.

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

`theme.rs` defines a `Tokens` resource: the only place colours, spacing (4, 8, 12, 16, 24 px), radii (4, 6, 8), the type scale (12, 14, 16, 20 px) and layout sizes (32 px hit target, 64 px tool rail, 320 px inspector, 48 px top bar, 28 px status bar) are written down. It has a light and a dark variant, chosen from `prefers-color-scheme` at start-up and swapped when that setting changes (a `change` listener on the media query feeds a channel that `theme::follow_scheme` drains), and `apply` pushes it into egui's style. World colours (sky, ground, buildings, selection and aim markers with their dark casing, region outline), the camera colours and the four-step overlap ramp are tokens too. Systems that baked a token colour into an asset react to a swap: the world recolours the sky and the building and plain-ground materials in place, camera bodies pick up the new type colours, and the coverage overlay repaints. The overlap ramp owns the warm hues (yellow to purple); camera types use cool and neutral hues so a cone never reads as a coverage step, and each type also has an icon so colour is never the only cue.

The UI font is Inter (with tabular digits frozen in, since egui cannot switch OpenType features on) or Iosevka, chosen in Settings and remembered in `localStorage`; Iosevka is always the monospace face. Both are subset to Latin-1 by `scripts/subset-fonts.sh` and embedded (about 100 KB together), with egui's bundled fonts kept as fallbacks. Icons are Phosphor (MIT) via `egui-phosphor`, subset at compile time in `icons.rs` to the glyphs the UI uses. egui has no font weights, so the semibold cut is its own family (`theme::strong_family`).

`web/index.html` mirrors the handful of values that the loading screen and the small-screen gate need as CSS custom properties, with a dark variant under `@media (prefers-color-scheme: dark)`; the file says to keep it in sync with `theme.rs`.

`scripts/check-design-tokens.sh` runs in CI. It scans every `.rs` file under the client's `src/` except `theme.rs` and fails on colour constructors (`Color32::from_rgb`, `Color::srgb(...)`, `Srgba::new(...)`, named `Color32::` constants and similar) or hex colour strings, so colours cannot drift away from the tokens.

## Viewport gate

The minimum supported viewport is 768 x 600 CSS px, enforced twice: a CSS media query in `index.html` (`max-width: 767.98px` or `max-height: 599.98px`) shows a static notice and hides the loader and canvas before the application is needed, and an in-app gate (`MIN_VIEWPORT` in `theme.rs`) replaces the UI with the same message if the window shrinks below the minimum at run time. The media query only hides the page and shows the notice; it does not stop the WebAssembly module from loading. Its value is that a phone user sees the explanation at once instead of a half-working UI.

## Persistence

The scene is serialised with `SceneFile` (`format: "observation-scene"`, `version: 1`) to `localStorage` under `observation.scene.v1` whenever its JSON changes, checked every 1.5 s. At start-up the stored scene is restored (and its area re-fetched); a missing entry loads the demo and opens the help card; an unreadable entry shows an error and falls back to the demo. Export and import use the same format through a file download and a file picker. `SceneFile::from_json` is the single validation point (see the threat model). Camera positions are relative to the stored origin, which comes from the bundle, so a saved scene lines up with its buildings as long as the area is regenerated from the same box. A camera's optional `group` is stored only when set by hand (trimmed, at most 80 characters, blank dropped on load); addresses are never saved, since they are worked out again from the area.

## Security and supply chain

### Workflows

All fifteen workflows set `permissions: {}` at the top and grant each job only what it needs. Every action is pinned to a full commit SHA, and checkouts use `persist-credentials: false`. Pushes, tags and pull requests that must start other workflows are made by the repository's GitHub App (CONTRIBUTING.md, "Bot app"), whose key is an environment secret of `bot` (develop, main and `v*` tags only); its token is minted in a job, or after the steps, that builds nothing, and git hooks are off while it is in use.

| Workflow | Triggers | What it does |
| --- | --- | --- |
| `ci.yml` | push to `main`, `develop`, `release/**`, `hotfix/**`; every pull request | `cargo fmt --check`; clippy with `-D warnings` for the native workspace and, separately, the client for `wasm32-unknown-unknown`; tests (cargo-nextest, plus doctests); the design-token guard and the toolchain version sync check; an informational coverage run (cargo-llvm-cov); a release `trunk build` with a `dist/` artifact (and, on pushes, a size report); Docker builds of the app image and the region agent image with smoke tests, run alongside the wasm build (the app image takes a stub `dist/` through Dockerfile target `runtime-prebuilt`, so Bevy is compiled once; the smoke test checks that the image runs as 65532, `/healthz` answers and `dist/index.html` is present, and the wasm job checks the real `dist/`). Rust and Docker layer caches are written only from `develop`, which every other branch and pull request can read; each Rust job keeps only the pinned toolchain installed, so rust-cache's key does not change with the runner image |
| `security.yml` | push to `main` and `develop`, pull requests, daily, manual | `cargo-deny` (advisories, bans, licences, sources); zizmor over `.github` (workflows and `dependabot.yml`, with its online audits; SARIF to code scanning, config in `.github/zizmor.yml`); `cargo-audit` (pinned binary) over both `Cargo.lock` and `fuzz/Cargo.lock`, failing on yanked crates too; Trivy filesystem scan (vulnerabilities, Dockerfile misconfiguration, secrets) and image scan, failing on fixable critical or high findings and uploading SARIF, and failing if the image scan sees no Rust crates in the server binary (the image scan uses a stub `dist/`, since Trivy does not inspect the static client) |
| `codeql.yml` | push and pull request to `main` and `develop`, weekly | CodeQL for `rust` and for the workflows themselves (`actions`); the Rust extractor's cargo target directory is cached (saved from `develop`) |
| `scorecard.yml` | push to whichever of `main`/`develop` is the default branch, weekly, branch-protection changes, manual | OpenSSF Scorecard, results published and uploaded to code scanning |
| `release.yml` | push to `develop`, tags `v*` | Builds and pushes `ghcr.io/panevain/observation` and `ghcr.io/panevain/observation-regions`: `devel` from `develop`, the version tag and `latest` from `v*` tags. Each image is smoke-tested before it is pushed; a tag must name the workspace version, be on `main` and pass cargo-deny's advisory check, and its images must pass Trivy (fixable critical or high) first. SLSA provenance (`mode=max`), an SBOM, a keyless cosign signature and a GitHub build-provenance attestation per image; the client is compiled inside the image build, not taken from CI, so provenance covers everything. A tag also gets a GitHub release (releases are immutable) with its changelog section and the image digests |
| `prune-images.yml` | weekly, after each Release run, manual (dry run by default) | For both images, deletes tagged versions whose tags are not `devel`, `latest` or `X.Y.Z`, and `sha256-<digest>` attestation tags whose image the registry no longer serves; untagged versions are never deleted |
| `fuzz.yml` | pull requests touching the fuzzed crates or `fuzz/`, push to `develop`, nightly, manual | cargo-fuzz (libFuzzer) on the untrusted-input parsers and coverage geometry: 30 s per target on PRs, 10 min nightly (1 min for `bbox_query`, 15 for `coverage_geometry`); the corpus is cached and grows night to night; crashing inputs uploaded as an artifact, and a nightly crash opens an issue. See [fuzz/README.md](../fuzz/README.md) |
| `image-full.yml` | pull requests touching the Dockerfile or build inputs, weekly, manual | Builds the default Dockerfile target, with the client compiled inside the image as Release does, and runs the same smoke test as the CI docker job. Not a required check |
| `treemap.yml` | tags `v*`, manual | Runs `scripts/wasm-size-report.sh` (a separate fat-LTO build) and opens a PR to `develop` (as the bot app) when `docs/images/wasm-bundle-treemap.svg` changed |
| `release-prep.yml` | manual | Starts a release: creates `release/<version>` from `develop`, bumps the version and `Cargo.lock`, dates the changelog, opens the PR into `main` (as the bot app) |
| `release-tag.yml` | push to `main` | Tags `main` with its workspace version when a `release/*` or `hotfix/*` PR merged (hotfixes after one approval of the `release` environment), which starts Release, then opens a back-merge PR into `develop` with auto-merge |
| `release-watch.yml` | daily, manual | Checks the latest release for crate advisories with a compatible fix (cargo-audit, then `cargo update`) and fixable OS vulnerabilities in its published images that a rebuild clears (Trivy); starts `hotfix.yml` when there is something to fix and keeps everything it finds in one issue per release |
| `hotfix.yml` | manual, started by `release-watch.yml` | Opens the security patch release of the latest release as `hotfix/X.Y.Z+1` into `main`: compatible crate fixes, re-pinned base images, the version bump and a changelog entry; the PR merges itself when its checks pass |
| `pins.yml` | weekly, manual | Bumps what Dependabot can't (`scripts/bump-pins.py`: the tools in `mise.toml` and `mise.ci.toml` through `mise upgrade --bump`, fnox, mise itself, Overpass, Rust, the fuzz nightly), each a week after its release, and refreshes both mise lockfiles, in one PR to `develop` |
| `branch-policy.yml` | pull requests | Fails PRs into `main` unless they come from a same-repo `release/*` or `hotfix/*` branch (git-flow) |

Branching, rulesets and the required checks are described in [CONTRIBUTING.md](../CONTRIBUTING.md); the rulesets themselves are versioned in `.github/rulesets/`.

**Dependabot** proposes weekly updates against `develop` (the default branch) for Cargo and GitHub Actions, grouped by minor and patch, after a 7-day cooldown that gives the community time to catch a bad release; security updates are grouped too and not delayed. Docker base-image digests are proposed daily without a cooldown, since that is how Debian fixes reach the images. Dependabot alerts and security updates are repository settings, not part of the config file, and only look at `develop`: the latest release is covered by `release-watch.yml`. **Pins Dependabot can't move** (the tools in `mise.toml` and `mise.ci.toml`, fnox, mise itself, Overpass, the Rust toolchain and the fuzz nightly) are bumped by `pins.yml`.

**Pinned tools.** Each tool is pinned once, through [mise](https://mise.jdx.dev): in `mise.toml` what developers use too (trunk, and task tools such as fnox), and in `mise.ci.toml` what only CI and the image build use (Trivy, cosign, cargo-audit, cargo-deny, zizmor, cargo-nextest, cargo-llvm-cov, cargo-fuzz, cargo-chef, cargo-auditable). `mise.lock` and `mise.ci.lock` record each release asset's SHA-256 for every platform, and mise checks the publisher's GitHub attestations or cosign signature where there is one; CI and the Dockerfile's build-tools stage install with `--locked`, so only those assets are downloaded. Trivy comes from its release instead of `aquasecurity/trivy-action`, whose tags were compromised in March 2026 (CVE-2026-33634). The workflows set `MISE_ENV=ci`, which loads `mise.ci.toml` on top of `mise.toml`: CI's environment (cargo, nextest and Trivy settings), and the tasks behind the security, fuzz and scan steps (`mise -E ci run <task>` runs one locally). Each job installs only the tools it uses, and the jobs that publish or prepare a release do so without mise-action's cache, so nothing they run comes from a cache. Rust itself is installed by rustup from `rust-toolchain.toml` (`fuzz/rust-toolchain.toml` for the fuzz targets, and a release's own for the release checks); `mise.ci.toml` unsets the `RUSTUP_TOOLCHAIN` that mise would otherwise export for the root toolchain.

**cargo-deny** (`deny.toml`): yanked crates and advisories are errors; only an allow-list of permissive licences passes; wildcard versions are denied (workspace path dependencies excepted); duplicate versions only warn, since Bevy's tree has many; crates must come from crates.io and git dependencies are denied. The graph is resolved for x86_64 and aarch64 Linux and wasm32.

**Accepted advisories.** An advisory may only be ignored with a written reason in `deny.toml`, mirrored in `.cargo/audit.toml` for cargo-audit and in `osv-scanner.toml` (with an `ignoreUntil` date) for OpenSSF Scorecard; `mise run sync` checks the three lists agree, and cargo-deny fails once an ignore no longer matches anything. Currently one: RUSTSEC-2026-0344 (decompression bomb in `ruzstd`). `ruzstd` 0.8 is a hard dependency of `bevy_image` 0.20 and the fix (0.9.1) is semver-incompatible; Bevy 0.20.1 is to move to it. The vulnerable decoder is only used for zstd-supercompressed KTX2 textures: `bevy_egui` turns on `bevy_image`'s `zstd_rust` feature, but the client leaves `ktx2` off (the only code that calls `ruzstd`) and `tonemapping_luts` off (using the LUT-free Khronos PBR Neutral tonemapper), and never loads untrusted textures.

**Container image.** Multi-stage. The builder is `rust:1.99.1-slim-trixie`, pinned by digest to match `rust-toolchain.toml`, and builds with `--locked` and `cargo auditable` (the binaries carry their dependency list, which Trivy, the SBOM and `cargo audit bin` read). The runtime is `gcr.io/distroless/cc-debian13:nonroot`, also pinned by digest, which has no shell and runs as uid and gid 65532. The region agent image is `debian:trixie-slim`, also pinned, with `apt-get upgrade` in its final stage (rebuilt without the layer cache for releases). Distroless stops rebuilding a Debian release's images once the next one is out, so the runtime moves with Debian. Release images can be verified with `gh attestation verify` or `cosign verify ghcr.io/panevain/observation:<tag> --certificate-identity-regexp '^https://github.com/panevain/observation/\.github/workflows/release\.yml@' --certificate-oidc-issuer https://token.actions.githubusercontent.com`; see [SECURITY.md](../SECURITY.md) for reporting and scope.

### Threat model

There are no accounts, sessions or personal data. The assets worth protecting are the server's availability, its standing with the upstream services, and the user's browser.

| Threat | Mitigations | Residual risk |
| --- | --- | --- |
| **Untrusted scene files** (imported JSON, or tampered `localStorage`) | Parsed with serde into typed structures; `format` and `version` checked; at most 256 cameras and 64 custom models; positions and angles must be finite and below 10,000; subject height 0-10 m; custom pixel density 1-2000 px/m; custom models validated (kebab-case id, field-of-view, resolution and IR ranges, and a `source` link that must be an `https://` URL with a host and no user info; a scene's non-https link is dropped on load, so a shared file cannot plant a `javascript:` or `data:` link and an older autosave still restores); labels and group names are drawn as text, never as markup (group names are trimmed and cut to 80 characters); a failed import changes nothing | No limit on the file's byte size; duplicate camera ids and unknown `model_id`s are not rejected (an unknown model just shows as "Unknown model" and is skipped); a stored bounding box is not range-checked on import, but the server validates it when the area is fetched |
| **Untrusted bounding-box input** (`/api/area`) | Strict parsing; ranges, ordering and 20-500 m size enforced server-side regardless of the UI; request body limit; coordinates are only ever formatted back as fixed-precision numbers into the query, so there is no injection into Overpass QL; cache file names come from a sanitised key | Distinct boxes make distinct cache entries, and bundles are never evicted or size-capped, so a client that sweeps many boxes can grow the cache directory |
| **Abuse of the proxy** (tile and Overpass amplification) | Upstream URLs are fixed by operator configuration, never by request data; tile coordinates validated, so no path traversal or arbitrary URLs; single-flight and caching for bundles; one Overpass request at a time, at most one attempt per configured interpreter (two for a single one) and a per-interpreter cooldown after 429 or 504; at most 4 upstream tile fetches; 512 MiB tile cap (oldest tiles evicted); PNG magic and size checks; 80 s timeout; responses cached by browsers for a day or a week | No per-client rate limiting. A flood of distinct boxes is cut off by the 3-deep Overpass queue (503s), but one client can still occupy that queue. A flood of distinct tiles can push the cached ones out, so they go upstream again (still limited to 4 at a time). Run it behind a reverse proxy that rate-limits per client |
| **Browser-side injection** | Strict CSP (self-only connections, no framing, no objects, no `base`), `nosniff`, `X-Frame-Options: DENY`; the UI is a canvas and draws all text through egui; the only data-driven link (a model's spec sheet) must be `https://`, checked on load and again before it is shown | CSP allows inline scripts and styles because of trunk's output; no HSTS from the app itself |
| **Supply chain** | Everything above: pinned SHAs and digests, checksum-verified tools, `--locked` builds, auditable binaries, cargo-deny, cargo-audit, Trivy, CodeQL, Scorecard, provenance and SBOM; security patch releases of the latest release when a fix appears | Dependencies still update through Dependabot and Pins PRs, so review them; a hotfix merges itself and only its tag needs an approval |
| **Data poisoning** (bad OSM data) | Bundles are rebuilt from a fresh parse; heights are clamped; degenerate geometry is dropped | OSM edits can change results; cached bundles live for the TTL (7 days) and can be served stale if upstream is down |

## Privacy and network traffic

Observation has no telemetry: no analytics, crash reporting, usage pings, update checks, accounts or cookies, in the app or the server. The only network traffic is what the feature in use needs, listed below. This was audited against the sources of the pinned dependencies and tools, and the CI-side opt-outs were verified against the real Trivy 0.76.0 and trunk 0.21.14 binaries.

### Runtime

| Component | Destination | When | What is sent | Can it be disabled or changed? |
| --- | --- | --- | --- | --- |
| Browser (page and static files) | The server that served the page | Page load | Ordinary HTTP request data (IP, `User-Agent`, `Referer`). No cookies are set; the scene lives in `localStorage` and never leaves the browser | No (it is the app). The server writes no access log (request tracing is at `DEBUG`, below the default `RUST_LOG=info`) |
| Browser (`net.rs`) | `/api/config`, `/api/area?bbox=...`, `/api/demo`, `/api/tiles/{z}/{x}/{y}.png` on the same origin | Start-up; when an area is loaded; while map tiles are shown | The chosen bounding box (about 20-500 m square) or tile coordinates | The demo street needs only `/api/demo` and the map tiles |
| Browser to any third party | None | Never automatically | Nothing. `Content-Security-Policy` has `default-src 'self'` and `connect-src 'self'`, and `img-src` allows only `'self' data: blob:`, so scripts, fonts, styles, images, fetch, XHR, WebSocket and beacons cannot reach another origin. `web/index.html` loads no external script, font or stylesheet. The client has no HTTP, WebSocket or analytics crate, and compiles in none of Bevy's `http`, `https`, `web_asset_cache`, `bevy_remote`, `bevy_dev_tools`, `sysinfo_plugin` or trace features | Already off |
| Browser (user-clicked links: model "spec sheet" (`https://` only), OSM copyright) | The linked site | Only when the user clicks | A new tab (`window.open`, target `_blank`). CSP does not restrict these, and `Referrer-Policy: strict-origin-when-cross-origin` sends the origin (not the path). `Cross-Origin-Opener-Policy: same-origin` denies the opened page a handle back to the app | Don't click them |
| Server, Overpass client | `OBS_OVERPASS_URL` (default `overpass-api.de`) | Bundle cache miss or expiry (default TTL 7 days) | `POST` form field `data` holding the query above with the snapped bounding box; `User-Agent: observation/<version> (+OBS_CONTACT)` | Not for live areas. Point it at your own instance, or serve only the demo street. `OBS_CONTACT` sets the contact string |
| Region agent (optional) | `download.geofabrik.de` (`OBS_REGIONS_CATALOG_URL`) | Opening the add list (index, cached a day); importing or refreshing a region | `GET` of the index, the extract and its boundary; `User-Agent: observation-regions/<version> (+OBS_CONTACT)` | Only runs when deployed; `OBS_REGIONS_REFRESH_DAYS=0` stops scheduled refreshes |
| Server, region agent (optional) | `OBS_REGIONS_URL` (your own agent) | Constantly (a long poll at most every 25 s), and on bundle misses inside a region | Bounding boxes and the Overpass query; the region list | Unset `OBS_REGIONS_URL` |
| Server, tile proxy | `OBS_TILE_URL` (default `tile.openstreetmap.org`) | Tile cache miss or expiry (at least 7 days) | `GET` of the tile; the same `User-Agent`; the browser's `Referer` header; `If-None-Match` / `If-Modified-Since` when revalidating | Point `OBS_TILE_URL` at another provider. The `Referer` is forwarded on purpose (see below) |
| Server, anything else | None | Never | No version check, tracing exporter or error reporting. Logs go to stdout only. The `metrics` and `metrics-exporter-prometheus` crates are present for opt-in Prometheus metrics: nothing is sent anywhere, they are served pull-only on the separate address `OBS_METRICS_BIND`, and with it unset (the default) no recorder is installed and no listener opens. `Cargo.lock` has no other telemetry crate (no sentry, opentelemetry, segment, posthog, mixpanel, datadog, honeycomb or self-update crate) | Already off (unset `OBS_METRICS_BIND`) |

Upstream services see the server's IP address and the server's `User-Agent`. They never see the visitor's IP address, `User-Agent` or any other header besides the `Referer`, because the browser only talks to the proxy. `reqwest` honours the standard `HTTP_PROXY`, `HTTPS_PROXY` and `ALL_PROXY` variables if the operator sets them.

**The forwarded `Referer`.** The OSM tile usage policy asks that browser apps not strip it, and it is what identifies the deployment to the tile operator. For a proxy it is a judgement call, and it is kept. The browser sends the page URL (`strict-origin-when-cross-origin` keeps the full URL on same-origin requests; the app uses no query string or path), so in practice it names the operator's own site. The server does not validate the header, so a page on another site that embeds `/api/tiles/...` images, or any script, can put its own URL there. Only the request that causes an upstream fetch forwards its `Referer`; cache hits send nothing upstream. An operator who wants to drop it can remove the `referer` argument in `routes/tiles.rs`, at the cost of departing from the policy's wording.

### Build, CI and development tools

| Tool | Destination | What is sent | Status |
| --- | --- | --- | --- |
| trunk 0.22.0 (Docker build, CI, local) | `crates.io` API | A once-per-interval check for a newer trunk | Off: `TRUNK_SKIP_VERSION_CHECK=true` in the `Dockerfile` builder stage and in `mise.toml`'s `[env]` (every build through mise, local or CI). It is a CLI option or environment variable only (not available in `Trunk.toml`), so set it in your own shell for builds without mise |
| trunk | `github.com` releases of wasm-bindgen and binaryen (`wasm-opt`) | Plain downloads | Required for the build. wasm-bindgen's version is taken from `Cargo.lock` (and so already pinned); `wasm-opt` defaults to `version_123`, fixed by the pinned trunk. Pinning them in `Trunk.toml` would add a second place to update. trunk 0.22's source (`src/tools.rs`) has no checksum check for these downloads, so treat them as trusted-by-TLS only |
| cargo, rustup | `crates.io` index and `static.rust-lang.org` | Package and toolchain downloads | No telemetry. CI and the Dockerfile pass `--no-self-update` to rustup |
| Trivy 0.76.1 (`security.yml`, `release.yml`, `release-watch.yml`, `hotfix.yml`) | `check.trivy.dev/updates` | Version check plus anonymous usage data | Off: `TRIVY_DISABLE_TELEMETRY=true` and `TRIVY_SKIP_VERSION_CHECK=true` (`[env]` in `mise.ci.toml`). Confirmed with the real binary: its debug log says "Version check and telemetry are disabled, skipping request" |
| Trivy | `ghcr.io/aquasecurity/trivy-db`, `mirror.gcr.io/aquasec/trivy-db` and `trivy-checks`; `release-watch.yml` and `hotfix.yml` also pull the published images and base images from `ghcr.io` and `gcr.io` | Downloads of the vulnerability database, rules and images | Required for scanning |
| `docker/build-push-action` | GitHub only | A build summary and a `.dockerbuild` build-record artifact on the workflow run | Left on: both stay on GitHub (`DOCKER_BUILD_SUMMARY` and `DOCKER_BUILD_RECORD_UPLOAD` default to true and can be set to false) |
| OpenSSF Scorecard (`scorecard.yml`) | `api.scorecard.dev` | The Scorecard results for this repository, signed with the workflow's OIDC token | Left on (`publish_results: true`): it feeds the README badge and makes the results public. Set it to `false` to keep results inside GitHub code scanning, at the cost of the badge |
| cosign 3.1.4 (`release.yml`) | Sigstore: Fulcio (certificate), Rekor (public transparency log), the TUF root CDN | The image digest and the workflow's OIDC identity, which Rekor publishes with the repository and workflow name | Required for signing; this is what makes the signature verifiable. Remove the signing step to stop it |
| zizmor 1.31.1 (`security.yml`) | `api.github.com` | The action references in the workflows (for the impostor-commit, known-vulnerable-action and stale-reference audits), with the job's read-only token | Left on: those audits need it. `--offline` turns them off |
| cargo-nextest, cargo-llvm-cov, cargo-audit, cargo-deny, cargo-fuzz | None of their own | Nothing (cargo-deny and cargo-audit fetch the RustSec advisory database, below) | No telemetry. None runs a self-update check in CI (`cargo nextest self update` is only run by hand) |
| mise (CI, the Dockerfile's build-tools stage, local) | `github.com` release assets; `api.github.com` and `mise-versions.jdx.dev` for version lists and attestation lookups | Downloads of the pinned assets. With `--locked` (CI and the Dockerfile) only the URLs in the lockfiles; resolving or locking versions (`mise run lock`, `mise upgrade`, the Pins workflow) also queries the release metadata | Install statistics for mise-versions are off (`use_versions_host_track = false` in `mise.toml`). mise-action downloads mise itself from its GitHub release and checks its signed checksums |
| `scripts/bump-pins.py` (`pins.yml`) | `api.github.com`, `static.rust-lang.org`, `dev.overpass-api.de`, Docker Hub and `ghcr.io`, plus mise's lookups above | Release and toolchain lookups, image digests; the Overpass tarball download | Required for the bumps |
| CodeQL, `upload-sarif`, Dependabot, cache and artifact actions | GitHub only (plus the RustSec advisory database for cargo-deny and cargo-audit) | Results, caches, advisory lookups | Required for the checks |

The README badges are images fetched by whoever views the README on GitHub (from `github.com` and `api.scorecard.dev`); they are not part of the app.

### Notes for the maintainer

- Hyperlinks use `bevy_egui`'s `open_url` feature (the `webbrowser` crate, which on wasm is a plain `window.open`, no network of its own). They open in a new tab so the app keeps its state, and the server's `Cross-Origin-Opener-Policy: same-origin` header severs `window.opener` because `webbrowser` does not pass `noopener`.
- Bevy's asset server has a `fetch`-based reader for `assets/` paths on the same origin in every wasm build. The client never calls `AssetServer::load`, so it is never used.
- Changing `Referrer-Policy` to `no-referrer` would also stop browsers sending any `Referer` to the proxy, which ends the forwarding described above. Treat the two as one decision.

## Testing

- **Core unit tests** cover projection round trips and axes, bounding-box parsing and snapping, tile maths, height and length parsing, extrusion winding and normals (with holes and raised parts), bundle round trips and rejection of bad input, road widths, DORI ranges, yaw and pitch conversion, catalogue parsing, view cones, exact occlusion cases, coverage on a small synthetic street, and scene file validation. An integration test (`crates/observation-core/tests/catalog.rs`) parses the shipped catalogue and requires a manufacturer `https://` source for every model that has a vendor.
- **Server unit tests** cover configuration parsing, cache key safety and TTL, Overpass parsing and ring stitching, tile path validation and the demo bundle decoding to a plausible number of buildings.
- **Fuzzing** (`fuzz/`, nightly toolchain) covers bundle decoding, scene-file import, Overpass parsing plus bundle building, bbox query parsing and the coverage geometry. Its first runs found two resource-exhaustion bugs on absurd geometry, now fixed with regression tests (see [fuzz/README.md](../fuzz/README.md)).
- **Server integration tests** (`crates/observation-server/tests/api.rs`) run the real router against **mock Overpass and tile servers** on loopback: security headers, `/api/config`, bad boxes, compression, fetch-once-then-cache, single-flight under concurrency, 503 and fast failure during the cooldown, the single retry on 504, 502 responses not being cached, serving expired bundles when upstream fails, tile proxying, tile validation without upstream calls, revalidation and stale-on-error for tiles, rejection of non-PNG tiles, endless chunked tile and Overpass bodies being cut off at the size cap, SPA fallback with cache headers, and JSON 404s for unknown API paths.
- **CI static checks**: formatting, clippy with warnings as errors on native and wasm targets, and the design-token guard.
- **Build checks**: the release `trunk build` and the Docker build with a smoke test.

The client has no unit tests yet. The CI step that would run them only runs if the client has a library target, which it currently does not, so client behaviour is covered by compilation, clippy and the core tests of the logic it uses.

## Known limitations and future work

- Flat-roofed boxes only: no pitched roofs, no building parts, no terrain, no vegetation, fences or vehicles. Roof geometry would need a different occlusion test.
- Coverage is computed on the main thread. A Web Worker, or a coarser first pass while dragging, would remove frame hitches for long-range cameras.
- The bundle cache has no size limit or eviction.
- Per-client rate limiting belongs in the server (or a proxy in front of it).
- Custom models cannot be edited or deleted in the app; there is no undo; there is a single autosave slot.
- Night modelling uses IR range only; it does not account for floodlights, street lighting or shutter behaviour.
- The client hard-codes its API paths and the 20-500 m limits rather than reading them from `/api/config`.
