use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use axum::http::{Method, StatusCode};
use observation_core::bundle::AreaBundle;
use observation_server::testing::TestApp;
use observation_upstream::testing::{FakeHttp, FakeReply};

use crate::harness::{OVERPASS, area_uri, flat_app, get_uri, hits, json, overpass_ok};
use crate::mocks::PNG;

/// Where the production configuration fetches elevation tiles.
const DEM_PREFIX: &str = "https://s3.amazonaws.com/elevation-tiles-prod/terrarium/";

/// A Terrarium tile 100 m up that rises 1 m every 8 pixel rows southward
/// (counted across tiles, so the mosaic is seamless).
fn dem_png(y: u32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, 256, 256);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header().unwrap();
    let mut data = Vec::with_capacity(256 * 256 * 3);
    for row in 0..256u32 {
        let e = 32768 + 100 + ((y % 64) * 256 + row) / 8;
        for _ in 0..256 {
            data.extend([(e >> 8) as u8, (e & 0xff) as u8, 0]);
        }
    }
    w.write_image_data(&data).unwrap();
    w.finish().unwrap();
    out
}

/// How the elevation server answers.
const TILES: u8 = 0;
const FAIL: u8 = 1;
/// A 1 x 1 PNG instead of a tile.
const TINY: u8 = 2;

/// An elevation server answering every tile as `mode` says.
fn dem(mode: &Arc<AtomicU8>) -> FakeHttp {
    let mode = Arc::clone(mode);
    FakeHttp::from_fn(move |req| {
        let path = req.url.strip_prefix(DEM_PREFIX).unwrap();
        match mode.load(Ordering::SeqCst) {
            FAIL => FakeReply::status(StatusCode::INTERNAL_SERVER_ERROR).body("boom"),
            TINY => FakeReply::ok(PNG),
            _ => {
                let y: u32 = path
                    .rsplit('/')
                    .next()
                    .unwrap()
                    .trim_end_matches(".png")
                    .parse()
                    .unwrap();
                FakeReply::ok(dem_png(y))
            }
        }
    })
}

/// The `z/x/y.png` paths asked of the elevation server.
fn paths(fake: &FakeHttp) -> Vec<String> {
    fake.requests()
        .into_iter()
        .map(|r| r.url.strip_prefix(DEM_PREFIX).unwrap().to_owned())
        .collect()
}

#[tokio::test(start_paused = true)]
async fn areas_are_built_on_the_terrain() {
    let cache = tempfile::tempdir().unwrap();
    let overpass = FakeHttp::new();
    overpass.on(Method::POST, OVERPASS, overpass_ok());
    let tiles = dem(&Arc::new(AtomicU8::new(TILES)));
    let app = TestApp::builder()
        .overpass(overpass.clone())
        .tiles(tiles.clone())
        .disk_cache(cache.path())
        .config(|c| c.dem_attribution = "Elevation: mock".into())
        .build();
    let (status, _, body) = get_uri(&app.router, &area_uri()).await;
    assert_eq!(status, StatusCode::OK);
    let b = AreaBundle::decode(&body).unwrap();
    let t = &b.terrain;
    assert!(!t.is_flat());
    assert_eq!(t.attribution, "Elevation: mock");
    assert!((100.0..2200.0).contains(&t.datum_m), "{}", t.datum_m);
    // About 3.2 m per pixel here, so ~0.04 m per metre southward (+Z).
    let at = |z: f32| t.height_at(observation_core::glam::Vec2::new(0.0, z));
    assert!(
        (at(100.0) - at(-100.0) - 7.8).abs() < 1.0,
        "{}",
        at(100.0) - at(-100.0)
    );
    let bases: Vec<f32> = b.buildings.iter().map(|b| b.footprint.base_m).collect();
    assert!(bases.iter().any(|&y| y > 1.0) && bases.iter().any(|&y| y < -1.0));
    let fetched = tiles.requests().len();
    assert_eq!(fetched, 2, "{:?}", paths(&tiles));
    assert!(paths(&tiles).iter().all(|p| p.starts_with("15/")));

    // The bundle is cached, and so are the tiles under dem/.
    let (_, _, again) = get_uri(&app.router, &area_uri()).await;
    assert_eq!(again, body);
    assert_eq!(tiles.requests().len(), fetched);
    assert_eq!(hits(&overpass, OVERPASS), 1);
    assert!(cache.path().join("dem/15").is_dir());
}

#[tokio::test(start_paused = true)]
async fn elevation_failures_fail_the_area() {
    let overpass = FakeHttp::new();
    overpass.on(Method::POST, OVERPASS, overpass_ok());
    let mode = Arc::new(AtomicU8::new(FAIL));
    let app = TestApp::builder()
        .overpass(overpass.clone())
        .tiles(dem(&mode))
        .build();
    let (status, _, body) = get_uri(&app.router, &area_uri()).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(
        json(&body)["error"]
            .as_str()
            .unwrap()
            .contains("elevation server"),
        "{}",
        String::from_utf8_lossy(&body)
    );
    // Elevation comes first, so Overpass was not asked for data to throw away.
    assert_eq!(hits(&overpass, OVERPASS), 0);
    // A tile that is a PNG but not a 256 x 256 Terrarium tile is not cached either.
    mode.store(TINY, Ordering::SeqCst);
    let (status, _, body) = get_uri(&app.router, &area_uri()).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(String::from_utf8_lossy(&body).contains("unusable"));
    // Nothing was cached: the next request tries again.
    mode.store(TILES, Ordering::SeqCst);
    let (status, _, _) = get_uri(&app.router, &area_uri()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(hits(&overpass, OVERPASS), 1);
}

#[tokio::test(start_paused = true)]
async fn without_elevation_areas_are_flat() {
    let overpass = FakeHttp::new();
    overpass.on(Method::POST, OVERPASS, overpass_ok());
    let app = flat_app(overpass, |_| {});
    let (status, _, body) = get_uri(&app.router, &area_uri()).await;
    assert_eq!(status, StatusCode::OK);
    let b = AreaBundle::decode(&body).unwrap();
    assert!(b.terrain.is_flat() && b.terrain.attribution.is_empty());
    assert!(b.buildings.iter().all(|b| b.footprint.base_m == 0.0));
    // The demo keeps its recorded terrain either way.
    let (_, _, demo) = get_uri(&app.router, "/api/demo").await;
    assert!(!AreaBundle::decode(&demo).unwrap().terrain.is_flat());
}
