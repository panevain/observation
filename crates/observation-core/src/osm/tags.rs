//! How OSM tags become heights and road widths.

/// Assumed storey height, metres.
pub const STOREY_M: f32 = 3.0;

/// Parses an OSM length value (`"12"`, `"12 m"`, `"12.5m"`, `"40'"`, `"40 ft"`, `"12'6\""`) to metres.
pub fn parse_length_m(raw: &str) -> Option<f32> {
    let s = raw.trim().replace(',', ".");
    if let Some((ft, rest)) = s.split_once('\'') {
        let ft: f32 = ft.trim().parse().ok()?;
        let inches = rest.trim().trim_end_matches('"').trim();
        let inches: f32 = if inches.is_empty() {
            0.0
        } else {
            inches.parse().ok()?
        };
        return Some(ft * 0.3048 + inches * 0.0254);
    }
    let lower = s.to_ascii_lowercase();
    let (num, factor) = if let Some(n) = lower.strip_suffix("ft") {
        (n, 0.3048)
    } else if let Some(n) = lower.strip_suffix('m') {
        (n, 1.0)
    } else {
        (lower.as_str(), 1.0)
    };
    let v: f32 = num.trim().parse().ok()?;
    (v.is_finite() && v >= 0.0).then_some(v * factor)
}

/// Default total height for a `building=*` value when no height tags exist.
pub fn default_height_for(building: &str) -> f32 {
    match building {
        "shed" | "garage" | "garages" | "carport" | "hut" | "roof" | "kiosk" | "cabin" => 3.0,
        "bungalow" | "static_caravan" => 4.0,
        "house" | "detached" | "semidetached_house" | "terrace" | "residential" | "farm" => 6.5,
        "apartments" | "dormitory" | "hotel" => 10.0,
        "church" | "cathedral" => 12.0,
        _ => 6.0,
    }
}

/// Infers `(min_height, height)` in metres from OSM tags.
///
/// Priority: `height` → `building:levels`(+`roof:levels`) → building-type default.
/// `min_height` / `building:min_level` give the base for building parts.
pub fn infer_heights<'a>(tag: impl Fn(&str) -> Option<&'a str>) -> (f32, f32) {
    let levels = |k: &str| {
        tag(k)
            .and_then(|v| v.trim().parse::<f32>().ok())
            .filter(|v| v.is_finite() && *v >= 0.0)
    };
    let min = tag("min_height")
        .and_then(parse_length_m)
        .or_else(|| levels("building:min_level").map(|l| l * STOREY_M))
        .unwrap_or(0.0);
    let height = tag("height")
        .and_then(parse_length_m)
        .filter(|h| *h > 0.5)
        .or_else(|| {
            levels("building:levels").filter(|l| *l > 0.0).map(|l| {
                let roof = levels("roof:levels").unwrap_or(0.0);
                (l + roof) * STOREY_M + if roof == 0.0 { 1.0 } else { 0.0 }
            })
        })
        .unwrap_or_else(|| default_height_for(tag("building").unwrap_or("yes")));
    let height = height.clamp(1.0, 300.0);
    (min.min(height - 0.5).max(0.0), height)
}

/// `highway=*` values for ways people walk or cycle on rather than drive.
pub const FOOT_WAYS: [&str; 5] = ["footway", "path", "pedestrian", "cycleway", "steps"];

/// Whether a road of this `highway=*` type can give a house its street name:
/// not motorways or their links, and of the ways people walk or cycle on, only
/// pedestrian streets.
pub(crate) fn names_addresses(highway: &str) -> bool {
    !(highway == "motorway"
        || highway.ends_with("_link")
        || (FOOT_WAYS.contains(&highway) && highway != "pedestrian"))
}

/// Estimated carriageway width from OSM `highway`, `lanes` and `width` tags.
/// Returns `None` for highway types that are not part of the street surface.
pub fn road_width_m(highway: &str, lanes: Option<&str>, width: Option<&str>) -> Option<f32> {
    let base = match highway {
        "motorway" | "trunk" => 14.0,
        "primary" => 10.0,
        "secondary" => 8.0,
        "tertiary" => 7.0,
        "residential" | "unclassified" => 6.0,
        "living_street" => 5.0,
        "service" => 4.0,
        h if FOOT_WAYS.contains(&h) => 2.0,
        "motorway_link" | "trunk_link" | "primary_link" | "secondary_link" | "tertiary_link" => 6.0,
        _ => return None,
    };
    if let Some(w) = width
        .and_then(crate::building::parse_length_m)
        .filter(|w| (1.0..60.0).contains(w))
    {
        return Some(w);
    }
    if let Some(l) = lanes
        .and_then(|l| l.trim().parse::<f32>().ok())
        .filter(|l| (1.0..12.0).contains(l))
    {
        return Some(l * 3.25);
    }
    Some(base)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn tags(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    fn infer(t: &HashMap<String, String>) -> (f32, f32) {
        infer_heights(|k| t.get(k).map(String::as_str))
    }

    #[test]
    fn lengths() {
        assert_eq!(parse_length_m("12"), Some(12.0));
        assert_eq!(parse_length_m("12.5 m"), Some(12.5));
        assert!((parse_length_m("10'").unwrap() - 3.048).abs() < 1e-4);
        assert!((parse_length_m("10 ft").unwrap() - 3.048).abs() < 1e-4);
        assert!((parse_length_m("5'6\"").unwrap() - 1.6764).abs() < 1e-3);
        assert_eq!(parse_length_m("tall"), None);
    }

    #[test]
    fn height_priority() {
        assert_eq!(
            infer(&tags(&[("building", "house"), ("height", "8")])),
            (0.0, 8.0)
        );
        assert_eq!(
            infer(&tags(&[("building", "house"), ("building:levels", "2")])),
            (0.0, 7.0)
        );
        assert_eq!(
            infer(&tags(&[
                ("building", "house"),
                ("building:levels", "2"),
                ("roof:levels", "1")
            ])),
            (0.0, 9.0)
        );
        assert_eq!(infer(&tags(&[("building", "garage")])), (0.0, 3.0));
        assert_eq!(infer(&tags(&[("building", "yes")])), (0.0, 6.0));
        assert_eq!(
            infer(&tags(&[
                ("building:part", "yes"),
                ("height", "10"),
                ("min_height", "4")
            ])),
            (4.0, 10.0)
        );
    }

    #[test]
    fn road_widths() {
        assert_eq!(road_width_m("residential", None, None), Some(6.0));
        assert_eq!(road_width_m("residential", Some("2"), None), Some(6.5));
        assert_eq!(road_width_m("primary", Some("4"), Some("12 m")), Some(12.0));
        assert_eq!(road_width_m("proposed", None, None), None);
    }
}
