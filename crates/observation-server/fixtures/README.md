# Demo fixture

`demo-bbox.txt` (`south,west,north,east`, ~299 m × 256 m of a residential street
in Juanita, Kirkland WA, USA), `demo-overpass.json` and the two elevation tiles
`demo-dem-15-5260-11429.png` and `demo-dem-15-5260-11430.png` back
`GET /api/demo`, the offline demo street the client loads on first run and that
the tests use.

Data © OpenStreetMap contributors, licensed under the
[ODbL 1.0](https://www.openstreetmap.org/copyright).

Retrieved 2026-10-31 with a single OSM API request,
`GET https://api.openstreetmap.org/api/0.6/map?bbox=-122.208,47.7075,-122.204,47.7098`,
and converted to the Overpass `out geom` shape with
`python3 -I osm_to_overpass.py map.osm > demo-overpass.json`. The result
contains building ways (with their `addr:*` tags), building multipolygon
relations and highway ways. The converter also keeps address nodes (`addr:housenumber` or
`addr:housename`), as the live query does: here 205 of them, from the King
County address import (199 inside a building, 6 just outside one).

To re-record against Overpass directly, run `./fetch.sh` (one request; wait
≥ 30 s before retrying a failure). It uses this query, with `{bbox}` taken from
`demo-bbox.txt`:

```
[out:json][timeout:25];
(
  way["building"]({bbox});
  relation["building"]["type"="multipolygon"]({bbox});
  way["highway"]({bbox});
  node["addr:housenumber"]({bbox});
  node["addr:housename"]({bbox});
);
out geom qt;
```

## Elevation tiles

`demo-dem-*.png` are the Terrarium tiles (zoom 15) under the demo street and its
150 m margin, from the AWS Open Data
[Terrain Tiles](https://registry.opendata.aws/terrain-tiles/), retrieved
2026-11-01 with `./fetch-dem.sh` (two requests; `OBS_DEM_URL` overrides the
template). Their source here is the USGS 3DEP 1/3 arc-second DEM (public
domain), "courtesy of the U.S. Geological Survey"; the set's credits are listed
in [its attribution page](https://github.com/tilezen/joerd/blob/master/docs/attribution.md).
The tile list in `fetch-dem.sh` must match `dem_tiles_for` on `demo-bbox.txt`;
the `demo_dem_tiles_match_the_bbox` test in `src/demo.rs` checks it.
