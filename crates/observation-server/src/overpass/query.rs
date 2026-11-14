//! The Overpass query sent for an area.

use observation_core::geo::BBox;

/// Builds the single query used per bounding box.
pub fn build_query(bbox: &BBox) -> String {
    let b = bbox.overpass();
    format!(
        "[out:json][timeout:25];\n(\n  way[\"building\"]({b});\n  relation[\"building\"][\"type\"=\"multipolygon\"]({b});\n  way[\"highway\"]({b});\n  node[\"addr:housenumber\"]({b});\n  node[\"addr:housename\"]({b});\n);\nout geom qt;\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_matches_spec() {
        let q = build_query(&BBox {
            south: 1.0,
            west: 2.0,
            north: 1.1,
            east: 2.1,
        });
        assert!(q.starts_with("[out:json][timeout:25];"));
        assert!(q.contains("way[\"building\"](1.0000000,2.0000000,1.1000000,2.1000000);"));
        assert!(q.contains("relation[\"building\"][\"type\"=\"multipolygon\"]"));
        assert!(q.contains("node[\"addr:housenumber\"](1.0000000,2.0000000,1.1000000,2.1000000);"));
        assert!(q.trim_end().ends_with("out geom qt;"));
    }

    /// The exact query text: interpreters and the region agent receive it,
    /// and changing it changes what every area contains.
    #[test]
    fn query_text_is_stable() {
        let q = build_query(&BBox {
            south: 47.7075,
            west: -122.208,
            north: 47.7098,
            east: -122.204,
        });
        let expected = [
            "[out:json][timeout:25];",
            "(",
            "  way[\"building\"](47.7075000,-122.2080000,47.7098000,-122.2040000);",
            "  relation[\"building\"][\"type\"=\"multipolygon\"](47.7075000,-122.2080000,47.7098000,-122.2040000);",
            "  way[\"highway\"](47.7075000,-122.2080000,47.7098000,-122.2040000);",
            "  node[\"addr:housenumber\"](47.7075000,-122.2080000,47.7098000,-122.2040000);",
            "  node[\"addr:housename\"](47.7075000,-122.2080000,47.7098000,-122.2040000);",
            ");",
            "out geom qt;",
            "",
        ];
        assert_eq!(q, expected.join("\n"));
    }
}
