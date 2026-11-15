//! Finding a place on the area map: a latitude and longitude pasted from a
//! map app, or a place name searched through the server's optional geocoder
//! (`/api/geocode`).
//!
//! Coordinates are parsed here, without any request. A place name is sent
//! only when the user submits it (Enter or Go), never while typing, and only
//! when the server says it has a geocoder.

use observation_core::geo::LatLon;
use std::fmt;

/// Furthest north or south the area map reaches (Web Mercator), degrees.
pub const MAP_MAX_LAT: f64 = 85.0;
/// Shortest place name sent to the server, in characters.
pub const MIN_QUERY_CHARS: usize = 2;
/// Longest place name sent when the server does not say, in characters.
pub const DEFAULT_MAX_QUERY_CHARS: usize = 200;
/// Map zoom for a place without a usable extent (street level).
pub const STREET_ZOOM: f64 = 17.0;

/// Why text could not be read as a latitude and longitude.
#[derive(Debug, Clone, PartialEq)]
pub enum CoordError {
    /// Nothing typed.
    Empty,
    /// Words or fewer than two numbers: not an attempt at coordinates (it
    /// may be a place name).
    NotCoordinates,
    /// More than two numbers, as with decimal commas.
    TooManyNumbers(usize),
    /// Something that is not part of a coordinate, such as a stray symbol.
    Unexpected(String),
    /// A number with both a minus sign and a hemisphere letter.
    SignAndHemisphere,
    /// Hemisphere letters on one number only, or both on the same axis.
    Hemispheres,
    /// Degrees, minutes and seconds that do not add up (minutes or seconds of
    /// 60 or more, a fractional part before a smaller unit).
    BadDms,
    /// The latitude is outside -90 to 90; `swapped` when the numbers would
    /// fit the other way round.
    Latitude { value: f64, swapped: bool },
    /// The longitude is outside -180 to 180.
    Longitude(f64),
    /// A valid latitude the area map cannot show.
    BeyondMap(f64),
}

impl fmt::Display for CoordError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty | Self::NotCoordinates => {
                write!(f, "Type a latitude and longitude, such as 51.5007, -0.1246")
            }
            Self::TooManyNumbers(n) => write!(
                f,
                "Found {n} numbers; type a latitude and a longitude only, with a point (not a comma) for decimals"
            ),
            Self::Unexpected(s) => write!(
                f,
                "\u{201c}{s}\u{201d} is not part of a latitude and longitude"
            ),
            Self::SignAndHemisphere => {
                write!(
                    f,
                    "Use either a minus sign or N/S/E/W on a number, not both"
                )
            }
            Self::Hemispheres => write!(
                f,
                "Give N or S for the latitude and E or W for the longitude, or leave both out"
            ),
            Self::BadDms => write!(
                f,
                "Degrees, minutes and seconds do not add up (minutes and seconds go from 0 to 59)"
            ),
            Self::Latitude {
                value,
                swapped: true,
            } => write!(
                f,
                "Latitude {value} is outside -90 to 90; put the latitude first, then the longitude"
            ),
            Self::Latitude { value, .. } => write!(f, "Latitude {value} is outside -90 to 90"),
            Self::Longitude(v) => write!(f, "Longitude {v} is outside -180 to 180"),
            Self::BeyondMap(v) => write!(
                f,
                "Latitude {v} is too near the pole: the map reaches {MAP_MAX_LAT}\u{b0} north and south"
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Unit {
    Deg,
    Min,
    Sec,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Hemi {
    N,
    S,
    E,
    W,
}

impl Hemi {
    fn is_lat(self) -> bool {
        matches!(self, Self::N | Self::S)
    }
    fn negative(self) -> bool {
        matches!(self, Self::S | Self::W)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Tok {
    /// A number, its text's sign and whether it has a fractional part.
    Num {
        value: f64,
        negative: bool,
        fraction: bool,
    },
    Unit(Unit),
    Hemi(Hemi),
    /// `,` or `;` between the two coordinates.
    Sep,
}

/// One coordinate as written.
#[derive(Debug, Clone, Copy)]
struct Coord {
    /// Absolute value in degrees.
    degrees: f64,
    negative: bool,
    hemi: Option<Hemi>,
}

/// Splits `s` into tokens. `Err(NotCoordinates)` for words (a run of two or
/// more letters, or any letter but N, S, E or W).
fn tokenise(s: &str) -> Result<Vec<Tok>, CoordError> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next_is_number = chars
            .get(i + 1)
            .is_some_and(|n| n.is_ascii_digit() || *n == '.');
        match c {
            c if c.is_whitespace() => i += 1,
            ',' | ';' => {
                out.push(Tok::Sep);
                i += 1;
            }
            // The degree sign, and the ordinal indicator and ring that some
            // keyboards and sites use for it.
            '\u{b0}' | 'º' | '\u{2da}' => {
                out.push(Tok::Unit(Unit::Deg));
                i += 1;
            }
            '\'' | '\u{2032}' | '\u{2019}' => {
                // `''` is a seconds mark typed as two primes.
                if chars.get(i + 1) == Some(&'\'') {
                    out.push(Tok::Unit(Unit::Sec));
                    i += 2;
                } else {
                    out.push(Tok::Unit(Unit::Min));
                    i += 1;
                }
            }
            '"' | '\u{2033}' | '\u{201d}' => {
                out.push(Tok::Unit(Unit::Sec));
                i += 1;
            }
            '+' | '-' | '\u{2212}' if next_is_number => {
                let (tok, len) = number(&chars[i + 1..], c != '+');
                out.push(tok);
                i += 1 + len;
            }
            c if c.is_ascii_digit() || (c == '.' && next_is_number) => {
                let (tok, len) = number(&chars[i..], false);
                out.push(tok);
                i += len;
            }
            c if c.is_alphabetic() => {
                let word_len = chars[i..].iter().take_while(|c| c.is_alphabetic()).count();
                let hemi = match c.to_ascii_uppercase() {
                    'N' => Some(Hemi::N),
                    'S' => Some(Hemi::S),
                    'E' => Some(Hemi::E),
                    'W' => Some(Hemi::W),
                    _ => None,
                };
                match hemi {
                    Some(h) if word_len == 1 => {
                        out.push(Tok::Hemi(h));
                        i += 1;
                    }
                    _ => return Err(CoordError::NotCoordinates),
                }
            }
            other => return Err(CoordError::Unexpected(other.to_string())),
        }
    }
    Ok(out)
}

/// Reads digits with at most one decimal point from the start of `chars`.
fn number(chars: &[char], negative: bool) -> (Tok, usize) {
    let mut len = 0;
    let mut seen_point = false;
    for c in chars {
        if c.is_ascii_digit() {
            len += 1;
        } else if *c == '.' && !seen_point {
            seen_point = true;
            len += 1;
        } else {
            break;
        }
    }
    let text: String = chars[..len].iter().collect();
    let value = text.parse::<f64>().unwrap_or(f64::NAN);
    let fraction = text.split_once('.').is_some_and(|(_, f)| !f.is_empty());
    (
        Tok::Num {
            value,
            negative,
            fraction,
        },
        len,
    )
}

/// Reads one coordinate at `toks[*at..]`: an optional hemisphere letter,
/// degrees (with an optional `°`, then optional minutes `'` and seconds
/// `"`), and a hemisphere letter after it if none came before.
fn coordinate(toks: &[Tok], at: &mut usize) -> Result<Coord, CoordError> {
    let peek = |at: usize| toks.get(at).copied();
    let mut hemi = None;
    if let Some(Tok::Hemi(h)) = peek(*at) {
        hemi = Some(h);
        *at += 1;
    }
    let Some(Tok::Num {
        value,
        negative,
        fraction,
    }) = peek(*at)
    else {
        return Err(CoordError::NotCoordinates);
    };
    *at += 1;
    let mut degrees = value;
    // Degrees, then optionally minutes and seconds, each in turn.
    if let Some(Tok::Unit(Unit::Deg)) = peek(*at) {
        *at += 1;
        let mut last_fraction = fraction;
        for (unit, scale) in [(Unit::Min, 60.0), (Unit::Sec, 3600.0)] {
            match (peek(*at), peek(*at + 1)) {
                (
                    Some(Tok::Num {
                        value,
                        negative: false,
                        fraction,
                    }),
                    Some(Tok::Unit(u)),
                ) if u == unit => {
                    if last_fraction || value >= 60.0 {
                        return Err(CoordError::BadDms);
                    }
                    degrees += value / scale;
                    last_fraction = fraction;
                    *at += 2;
                }
                _ => break,
            }
        }
    }
    if let Some(Tok::Unit(_)) = peek(*at) {
        return Err(CoordError::BadDms);
    }
    if hemi.is_none()
        && let Some(Tok::Hemi(h)) = peek(*at)
    {
        hemi = Some(h);
        *at += 1;
    }
    if negative && hemi.is_some() {
        return Err(CoordError::SignAndHemisphere);
    }
    Ok(Coord {
        degrees,
        negative,
        hemi,
    })
}

impl Coord {
    fn value(self) -> f64 {
        if self.negative || self.hemi.is_some_and(Hemi::negative) {
            -self.degrees
        } else {
            self.degrees
        }
    }
}

/// Reads a latitude and longitude as map apps copy them: decimal degrees
/// (`51.5007, -0.1246`, `51.5007 -0.1246`, `51.5007°, -0.1246°`), with
/// hemisphere letters (`51.5007° N, 0.1246° W`, `N 51.5007 W 0.1246`, in
/// either order) or in degrees, minutes and seconds (`51°30'02.5"N
/// 0°07'28.6"W`), optionally as a `geo:` URI. The latitude comes first
/// unless hemisphere letters say otherwise. Anything ambiguous is refused
/// rather than guessed.
pub fn parse_lat_lon(input: &str) -> Result<LatLon, CoordError> {
    let mut s = input.trim();
    if let Some(rest) = s
        .get(..4)
        .filter(|p| p.eq_ignore_ascii_case("geo:"))
        .and_then(|_| s.get(4..))
    {
        s = rest;
        // `geo:` URIs may carry `;u=…` or `?q=…` after the position.
        s = s.split([';', '?']).next().unwrap_or("");
    }
    if s.trim().is_empty() {
        return Err(CoordError::Empty);
    }
    let toks = tokenise(s)?;
    let numbers = toks.iter().filter(|t| matches!(t, Tok::Num { .. })).count();
    if numbers < 2 {
        return Err(CoordError::NotCoordinates);
    }
    let mut at = 0;
    let first = coordinate(&toks, &mut at)?;
    if let Some(Tok::Sep) = toks.get(at) {
        at += 1;
    }
    let second = match coordinate(&toks, &mut at) {
        Ok(c) => c,
        Err(CoordError::NotCoordinates) => return Err(stray(&toks[at..], numbers)),
        Err(e) => return Err(e),
    };
    if at < toks.len() {
        return Err(stray(&toks[at..], numbers));
    }
    let (lat, lon) = match (first.hemi, second.hemi) {
        (None, None) => (first, second),
        (Some(a), Some(b)) if a.is_lat() && !b.is_lat() => (first, second),
        (Some(a), Some(b)) if !a.is_lat() && b.is_lat() => (second, first),
        _ => return Err(CoordError::Hemispheres),
    };
    let (lat, lon) = (lat.value(), lon.value());
    if !(-90.0..=90.0).contains(&lat) {
        let swapped = first.hemi.is_none()
            && (-90.0..=90.0).contains(&lon)
            && (-180.0..=180.0).contains(&lat);
        return Err(CoordError::Latitude {
            value: lat,
            swapped,
        });
    }
    if !(-180.0..=180.0).contains(&lon) {
        return Err(CoordError::Longitude(lon));
    }
    if lat.abs() > MAP_MAX_LAT {
        return Err(CoordError::BeyondMap(lat));
    }
    Ok(LatLon::new(lat, lon))
}

/// The error for tokens left over after the second coordinate.
fn stray(rest: &[Tok], numbers: usize) -> CoordError {
    if numbers > 2 {
        CoordError::TooManyNumbers(numbers)
    } else {
        match rest.first() {
            Some(Tok::Sep) => CoordError::Unexpected(",".into()),
            Some(Tok::Hemi(_)) => CoordError::Hemispheres,
            _ => CoordError::BadDms,
        }
    }
}

/// Encodes `s` for a URL query value: unreserved characters as they are,
/// every other byte of its UTF-8 as `%XX`.
pub fn encode_query_value(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// One place the server's geocoder found.
#[derive(Debug, Clone, PartialEq)]
pub struct Place {
    pub label: String,
    pub at: LatLon,
    /// `[south, west, north, east]`, when known.
    pub bbox: Option<[f64; 4]>,
}

impl Place {
    /// A zoom that shows the place's extent on the map, between town level
    /// and [`STREET_ZOOM`]; [`STREET_ZOOM`] without an extent.
    pub fn zoom(&self) -> f64 {
        let Some([s, w, n, e]) = self.bbox else {
            return STREET_ZOOM;
        };
        // The larger side, in degrees of longitude at this latitude.
        let span = (e - w).max((n - s) / self.at.lat.to_radians().cos().max(0.01));
        if span <= 0.0 {
            return STREET_ZOOM;
        }
        // At zoom z a 256-pixel tile spans 360 / 2^z degrees; aim for the
        // extent to fill about three tiles.
        (360.0 * 3.0 / span).log2().floor().clamp(12.0, STREET_ZOOM)
    }
}

/// What submitting the field asks the client to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Submit {
    /// Recentre the map here.
    GoTo(LatLon),
    /// A search was queued for the server ([`PlaceFinder::take_request`]).
    Searching,
    /// Nothing to do; [`PlaceFinder::message`] says why, if anything.
    Nothing,
}

/// The map dialog's "find a place" field, the search in flight and its
/// results.
#[derive(Debug, Default)]
pub struct PlaceFinder {
    /// The text in the field.
    pub text: String,
    /// What the field says under it: an error, or no results.
    message: Option<String>,
    results: Vec<Place>,
    /// Credit for the results, from the server.
    attribution: Option<String>,
    /// The query in flight.
    pending: Option<String>,
    /// The query to send, taken by the network code.
    outbox: Option<String>,
}

impl PlaceFinder {
    /// Reads the field when the user submits it. `search` is the server's
    /// longest query when it has a geocoder, `None` when it has none.
    pub fn submit(&mut self, search: Option<usize>) -> Submit {
        if self.pending.is_some() {
            return Submit::Nothing;
        }
        let text = self.text.trim();
        match parse_lat_lon(text) {
            Ok(at) => {
                self.message = None;
                self.results.clear();
                return Submit::GoTo(at);
            }
            // Perhaps a place name: searched below if the server can.
            Err(CoordError::NotCoordinates) => {}
            Err(CoordError::Empty) => {
                self.message = Some(match search {
                    Some(_) => "Type a place, or a latitude and longitude".to_owned(),
                    None => CoordError::Empty.to_string(),
                });
                return Submit::Nothing;
            }
            Err(e) => {
                self.message = Some(e.to_string());
                return Submit::Nothing;
            }
        }
        let Some(max) = search else {
            self.message = Some(CoordError::NotCoordinates.to_string());
            return Submit::Nothing;
        };
        let query = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let n = query.chars().count();
        if n < MIN_QUERY_CHARS || n > max {
            self.message = Some(format!(
                "Type {MIN_QUERY_CHARS} to {max} characters to search for a place"
            ));
            return Submit::Nothing;
        }
        self.message = None;
        self.results.clear();
        self.attribution = None;
        self.pending = Some(query.clone());
        self.outbox = Some(query);
        Submit::Searching
    }

    /// The query to send now, if the user just submitted one.
    pub fn take_request(&mut self) -> Option<String> {
        self.outbox.take()
    }

    /// The URL of the search for `query`.
    pub fn url(query: &str) -> String {
        format!("/api/geocode?q={}", encode_query_value(query))
    }

    /// The server's reply to the search for `query`: its body, or why it
    /// failed (already worded). A reply to anything but the search in flight
    /// is ignored.
    pub fn answer(&mut self, query: &str, reply: Result<Vec<u8>, String>) {
        if self.pending.as_deref() != Some(query) {
            return;
        }
        self.pending = None;
        let parsed = reply.and_then(|body| {
            parse_answer(&body).ok_or_else(|| "the server's answer could not be read".to_owned())
        });
        match parsed {
            Ok((places, attribution)) => {
                self.message = places
                    .is_empty()
                    .then(|| format!("No places found for \u{201c}{query}\u{201d}"));
                self.results = places;
                self.attribution = attribution;
            }
            Err(e) => self.message = Some(format!("Search failed: {e}")),
        }
    }

    /// A search is in flight.
    pub fn searching(&self) -> bool {
        self.pending.is_some()
    }

    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    pub fn results(&self) -> &[Place] {
        &self.results
    }

    /// Credit for the results shown.
    pub fn attribution(&self) -> Option<&str> {
        self.attribution.as_deref()
    }

    /// Clears the results and the message (the text stays).
    pub fn clear(&mut self) {
        self.results.clear();
        self.attribution = None;
        self.message = None;
    }
}

/// The places and credit in a `/api/geocode` body; places without a valid
/// position are dropped.
fn parse_answer(body: &[u8]) -> Option<(Vec<Place>, Option<String>)> {
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    let places = v
        .get("results")?
        .as_array()?
        .iter()
        .filter_map(|p| {
            let lat = p.get("lat")?.as_f64().filter(|l| l.abs() <= MAP_MAX_LAT)?;
            let lon = p.get("lon")?.as_f64().filter(|l| l.abs() <= 180.0)?;
            let label = p.get("label")?.as_str()?.to_owned();
            let bbox = p.get("bbox").and_then(|b| {
                let b = b.as_array()?;
                let n = |i: usize| b.get(i)?.as_f64().filter(|x| x.is_finite());
                Some([n(0)?, n(1)?, n(2)?, n(3)?])
            });
            Some(Place {
                label,
                at: LatLon::new(lat, lon),
                bbox,
            })
        })
        .collect();
    let attribution = v
        .get("attribution")
        .and_then(serde_json::Value::as_str)
        .filter(|a| !a.is_empty())
        .map(str::to_owned);
    Some((places, attribution))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(s: &str) -> (f64, f64) {
        // The error is not printed: its text can carry the coordinates.
        let ll = parse_lat_lon(s).unwrap_or_else(|_| panic!("{s:?} was refused"));
        (ll.lat, ll.lon)
    }

    fn close(a: (f64, f64), b: (f64, f64)) -> bool {
        (a.0 - b.0).abs() < 1e-4 && (a.1 - b.1).abs() < 1e-4
    }

    #[test]
    fn pairs_copied_from_map_apps_are_read() {
        let westminster = (51.5007, -0.1246);
        for s in [
            "51.5007, -0.1246",
            "51.5007,-0.1246",
            "  51.5007 ,  -0.1246  ",
            "51.5007 -0.1246",
            "51.5007\t-0.1246",
            "51.5007; -0.1246",
            "51.5007°, -0.1246°",
            "51.5007° -0.1246°",
            "+51.5007, \u{2212}0.1246",
            "51.5007° N, 0.1246° W",
            "51.5007N 0.1246W",
            "N 51.5007, W 0.1246",
            "n51.5007 w0.1246",
            "0.1246° W, 51.5007° N",
            "51°30'02.5\"N 0°07'28.6\"W",
            "51° 30′ 2.52″ N, 0° 7′ 28.56″ W",
            "51°30.042'N 0°7.476'W",
            "geo:51.5007,-0.1246",
            "GEO:51.5007,-0.1246;u=35",
            "geo:51.5007,-0.1246?z=17",
        ] {
            assert!(close(ok(s), westminster), "{s:?} gave {:?}", ok(s));
        }
        assert_eq!(ok("47.70865, -122.206"), (47.70865, -122.206));
        assert_eq!(ok("0, 0"), (0.0, 0.0));
        assert_eq!(ok("-33.8568 151.2153"), (-33.8568, 151.2153));
        assert_eq!(ok("85, 180"), (85.0, 180.0));
        assert_eq!(ok(".5 .25"), (0.5, 0.25));
    }

    #[test]
    fn words_and_single_numbers_are_not_coordinates() {
        for s in [
            "London",
            "10 Downing Street",
            "SW1A 2AA",
            "N1",
            "90210",
            "51.5",
            "news",
            "Zürich 8001",
            "東京都 1",
        ] {
            assert_eq!(parse_lat_lon(s), Err(CoordError::NotCoordinates), "{s:?}");
        }
        assert_eq!(parse_lat_lon("   "), Err(CoordError::Empty));
        assert_eq!(parse_lat_lon("geo:"), Err(CoordError::Empty));
    }

    #[test]
    fn ambiguous_or_impossible_pairs_are_refused_with_a_reason() {
        let cases: &[(&str, CoordError)] = &[
            ("51,5007, -0,1246", CoordError::TooManyNumbers(4)),
            ("51,5 -0,12", CoordError::TooManyNumbers(4)),
            ("51.5 -0.12 3", CoordError::TooManyNumbers(3)),
            ("51.5 # -0.12", CoordError::Unexpected("#".into())),
            ("-51.5 N, 0.12 W", CoordError::SignAndHemisphere),
            ("51.5 N, 0.12", CoordError::Hemispheres),
            ("51.5 N, 0.12 S", CoordError::Hemispheres),
            ("51.5 E, 0.12 W", CoordError::Hemispheres),
            ("51°60'N 0°7'W", CoordError::BadDms),
            ("51.5°30'N 0°7'W", CoordError::BadDms),
            ("51' 0.12", CoordError::BadDms),
            (
                "-122.206, 47.70865",
                CoordError::Latitude {
                    value: -122.206,
                    swapped: true,
                },
            ),
            (
                "200, 300",
                CoordError::Latitude {
                    value: 200.0,
                    swapped: false,
                },
            ),
            ("45, 181", CoordError::Longitude(181.0)),
            ("89.5, 10", CoordError::BeyondMap(89.5)),
            (
                "95 N, 10 E",
                CoordError::Latitude {
                    value: 95.0,
                    swapped: false,
                },
            ),
        ];
        for (s, want) in cases {
            assert_eq!(&parse_lat_lon(s).unwrap_err(), want, "{s:?}");
        }
    }

    #[test]
    fn every_error_reads_as_a_sentence() {
        let swapped = CoordError::Latitude {
            value: -122.2,
            swapped: true,
        };
        assert_eq!(
            swapped.to_string(),
            "Latitude -122.2 is outside -90 to 90; put the latitude first, then the longitude"
        );
        assert_eq!(
            CoordError::TooManyNumbers(4).to_string(),
            "Found 4 numbers; type a latitude and a longitude only, with a point (not a comma) for decimals"
        );
        assert_eq!(
            CoordError::NotCoordinates.to_string(),
            "Type a latitude and longitude, such as 51.5007, -0.1246"
        );
    }

    #[test]
    fn query_values_are_percent_encoded() {
        assert_eq!(
            encode_query_value("High St, Zürich&x=1"),
            "High%20St%2C%20Z%C3%BCrich%26x%3D1"
        );
        assert_eq!(PlaceFinder::url("a-b_c.d~"), "/api/geocode?q=a-b_c.d~");
    }

    #[test]
    fn coordinates_recentre_without_a_request() {
        let mut f = PlaceFinder {
            text: "51.5007, -0.1246".into(),
            ..PlaceFinder::default()
        };
        assert_eq!(
            f.submit(Some(200)),
            Submit::GoTo(LatLon::new(51.5007, -0.1246))
        );
        assert_eq!(f.take_request(), None);
        f.text = "91, 0".into();
        assert_eq!(f.submit(None), Submit::Nothing);
        assert!(f.message().unwrap().starts_with("Latitude 91 is outside"));
    }

    #[test]
    fn without_a_geocoder_names_are_explained_not_sent() {
        let mut f = PlaceFinder {
            text: "Kirkland".into(),
            ..PlaceFinder::default()
        };
        assert_eq!(f.submit(None), Submit::Nothing);
        assert_eq!(
            f.message(),
            Some("Type a latitude and longitude, such as 51.5007, -0.1246")
        );
        assert_eq!(f.take_request(), None);
    }

    #[test]
    fn a_name_is_searched_once_on_submit_and_its_answer_shown() {
        let mut f = PlaceFinder {
            text: "  High   Street  ".into(),
            ..PlaceFinder::default()
        };
        assert_eq!(f.submit(Some(200)), Submit::Searching);
        assert!(f.searching());
        assert_eq!(f.take_request().as_deref(), Some("High Street"));
        assert_eq!(f.take_request(), None, "sent once");
        // A second submit while waiting sends nothing.
        assert_eq!(f.submit(Some(200)), Submit::Nothing);
        assert_eq!(f.take_request(), None);

        // A reply to another query is ignored.
        f.answer("Other", Ok(b"{\"results\": []}".to_vec()));
        assert!(f.searching());

        let body = br#"{"results": [
            {"label": "High Street, Kirkland", "lat": 47.7, "lon": -122.2,
             "bbox": [47.69, -122.21, 47.71, -122.19]},
            {"label": "Bad", "lat": 95, "lon": 0},
            {"label": "No box", "lat": 1.5, "lon": 2.5, "bbox": null}
        ], "attribution": "Search: somebody"}"#;
        f.answer("High Street", Ok(body.to_vec()));
        assert!(!f.searching());
        assert_eq!(f.message(), None);
        assert_eq!(f.attribution(), Some("Search: somebody"));
        let labels: Vec<_> = f.results().iter().map(|p| p.label.as_str()).collect();
        assert_eq!(labels, ["High Street, Kirkland", "No box"]);
        assert_eq!(f.results()[0].bbox, Some([47.69, -122.21, 47.71, -122.19]));
        assert_eq!(f.results()[1].bbox, None);

        f.clear();
        assert!(f.results().is_empty() && f.attribution().is_none());
    }

    #[test]
    fn empty_failed_and_bad_searches_say_why() {
        let mut f = PlaceFinder {
            text: "x".into(),
            ..PlaceFinder::default()
        };
        assert_eq!(f.submit(Some(200)), Submit::Nothing);
        assert_eq!(
            f.message(),
            Some("Type 2 to 200 characters to search for a place")
        );
        f.text = "abcdef".into();
        assert_eq!(f.submit(Some(5)), Submit::Nothing);
        f.text = String::new();
        assert_eq!(f.submit(Some(200)), Submit::Nothing);
        assert_eq!(
            f.message(),
            Some("Type a place, or a latitude and longitude")
        );

        f.text = "Nowhere".into();
        assert_eq!(f.submit(Some(200)), Submit::Searching);
        f.answer("Nowhere", Ok(br#"{"results": []}"#.to_vec()));
        assert_eq!(
            f.message(),
            Some("No places found for \u{201c}Nowhere\u{201d}")
        );

        assert_eq!(f.submit(Some(200)), Submit::Searching);
        f.answer(
            "Nowhere",
            Err("too many place searches from your address; try again in about 4 s".into()),
        );
        assert_eq!(
            f.message(),
            Some(
                "Search failed: too many place searches from your address; try again in about 4 s"
            )
        );
        assert_eq!(f.submit(Some(200)), Submit::Searching);
        f.answer("Nowhere", Ok(b"<html>".to_vec()));
        assert_eq!(
            f.message(),
            Some("Search failed: the server's answer could not be read")
        );
    }

    #[test]
    fn a_place_is_shown_at_a_zoom_that_fits_it() {
        let at = LatLon::new(47.7, -122.2);
        let place = |bbox| Place {
            label: String::new(),
            at,
            bbox,
        };
        assert_eq!(place(None).zoom(), STREET_ZOOM);
        // A house: street level.
        assert_eq!(
            place(Some([47.6999, -122.2001, 47.7001, -122.1999])).zoom(),
            STREET_ZOOM
        );
        // A neighbourhood about 2 km across.
        let z = place(Some([47.69, -122.215, 47.71, -122.185])).zoom();
        assert!((14.0..=15.0).contains(&z), "{z}");
        // A country: no further out than town level.
        assert_eq!(place(Some([45.5, -124.8, 49.0, -116.9])).zoom(), 12.0);
    }
}
