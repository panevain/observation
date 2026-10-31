# Demo fixture

`demo-bbox.txt` (`south,west,north,east`, ~299 m × 256 m of a residential street
in Juanita, Kirkland WA, USA) and `demo-overpass.json` back `GET /api/demo`, the
offline demo street the client loads on first run and that the tests use.

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
