#!/usr/bin/env python3
"""Convert an OSM API `/map` XML extract into the Overpass `out geom` JSON
shape served by /api/demo.

Keeps building ways, building multipolygon relations, highway ways and
address nodes, i.e. the same feature set as the server's Overpass query.

Usage: python3 -I osm_to_overpass.py map.osm [TIMESTAMP] > demo-overpass.json

TIMESTAMP (`YYYY-MM-DDTHH:MM:SSZ`, UTC) is when the extract was downloaded,
written as `osm3s.timestamp_osm_base` like Overpass's own data date: the OSM
API serves live data, so that is when the data was current. It defaults to
now; give the download time when converting an older extract.
"""
import json
import re
import sys
import xml.etree.ElementTree as ET
from datetime import datetime, timezone


def main(path, timestamp=None):
    if timestamp is None:
        timestamp = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    if not re.fullmatch(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z", timestamp):
        sys.exit(f"timestamp must look like 2026-10-31T19:43:02Z, not {timestamp!r}")
    root = ET.parse(path).getroot()
    nodes = {n.get("id"): (float(n.get("lat")), float(n.get("lon"))) for n in root.iter("node")}
    ways = {}
    for w in root.iter("way"):
        refs = [nd.get("ref") for nd in w.iter("nd")]
        tags = {t.get("k"): t.get("v") for t in w.iter("tag")}
        geom = [{"lat": nodes[r][0], "lon": nodes[r][1]} for r in refs if r in nodes]
        ways[w.get("id")] = (tags, geom, len(geom) == len(refs))
    elements = []
    for n in root.iter("node"):
        tags = {t.get("k"): t.get("v") for t in n.iter("tag")}
        if "addr:housenumber" in tags or "addr:housename" in tags:
            elements.append({"type": "node", "id": int(n.get("id")), "lat": float(n.get("lat")),
                             "lon": float(n.get("lon")), "tags": tags})
    for wid, (tags, geom, complete) in ways.items():
        if complete and ("building" in tags or "highway" in tags):
            elements.append({"type": "way", "id": int(wid), "geometry": geom, "tags": tags})
    for r in root.iter("relation"):
        tags = {t.get("k"): t.get("v") for t in r.iter("tag")}
        if tags.get("type") != "multipolygon" or "building" not in tags:
            continue
        members = []
        for m in r.iter("member"):
            if m.get("type") == "way" and m.get("ref") in ways:
                members.append({"type": "way", "ref": int(m.get("ref")), "role": m.get("role"),
                                "geometry": ways[m.get("ref")][1]})
        elements.append({"type": "relation", "id": int(r.get("id")), "members": members, "tags": tags})
    out = {
        "version": 0.6,
        "generator": "osm_to_overpass.py (from OSM API 0.6 /map extract)",
        "osm3s": {"timestamp_osm_base": timestamp,
                  "copyright": "The data included in this document is from www.openstreetmap.org. "
                               "The data is made available under ODbL."},
        "elements": elements,
    }
    json.dump(out, sys.stdout, separators=(",", ":"))


if __name__ == "__main__":
    if len(sys.argv) not in (2, 3):
        sys.exit(__doc__)
    main(*sys.argv[1:])
