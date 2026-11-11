use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{Response, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get;
use observation_core::bundle::AreaBundle;

use crate::harness::{area_uri, get_uri, harness, harness_with, json};
use crate::mocks::{PNG, spawn, spawn_overpass};

#[derive(Default)]
struct MockDem {
    hits: AtomicUsize,
    fail: AtomicBool,
    /// Answer with a 1 x 1 PNG instead of a tile.
    tiny: AtomicBool,
    paths: Mutex<Vec<String>>,
}

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

async fn dem_handler(
    State(m): State<Arc<MockDem>>,
    Path((z, x, y)): Path<(String, String, String)>,
) -> Response<Body> {
    m.hits.fetch_add(1, Ordering::SeqCst);
    m.paths.lock().unwrap().push(format!("{z}/{x}/{y}"));
    if m.fail.load(Ordering::SeqCst) {
        return (StatusCode::INTERNAL_SERVER_ERROR, "boom").into_response();
    }
    if m.tiny.load(Ordering::SeqCst) {
        return ([(header::CONTENT_TYPE, "image/png")], PNG).into_response();
    }
    let y: u32 = y.trim_end_matches(".png").parse().unwrap();
    Response::builder()
        .header(header::CONTENT_TYPE, "image/png")
        .body(Body::from(dem_png(y)))
        .unwrap()
}

async fn spawn_dem() -> (Arc<MockDem>, String) {
    let mock = Arc::new(MockDem::default());
    let router = Router::new()
        .route("/terrarium/{z}/{x}/{y}", get(dem_handler))
        .with_state(Arc::clone(&mock));
    let addr = spawn(router).await;
    (
        mock,
        format!("http://{addr}/terrarium/{{z}}/{{x}}/{{y}}.png"),
    )
}

#[tokio::test]
async fn areas_are_built_on_the_terrain() {
    let (overpass, url) = spawn_overpass().await;
    let (dem, dem_url) = spawn_dem().await;
    let h = harness_with(
        &url,
        "http://127.0.0.1:9/{z}/{x}/{y}.png",
        tempfile::tempdir().unwrap(),
        |c| {
            c.dem_url = Some(dem_url);
            c.dem_attribution = "Elevation: mock".into();
        },
    );
    let (status, _, body) = get_uri(&h.app, &area_uri()).await;
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
    let fetched = dem.hits.load(Ordering::SeqCst);
    assert_eq!(fetched, 2, "{:?}", dem.paths.lock().unwrap());
    assert!(
        dem.paths
            .lock()
            .unwrap()
            .iter()
            .all(|p| p.starts_with("15/"))
    );

    // The bundle is cached, and so are the tiles under dem/.
    let (_, _, again) = get_uri(&h.app, &area_uri()).await;
    assert_eq!(again, body);
    assert_eq!(dem.hits.load(Ordering::SeqCst), fetched);
    assert_eq!(overpass.hits.load(Ordering::SeqCst), 1);
    assert!(h.cache.path().join("dem/15").is_dir());
}

#[tokio::test]
async fn elevation_failures_fail_the_area() {
    let (overpass, url) = spawn_overpass().await;
    let (dem, dem_url) = spawn_dem().await;
    dem.fail.store(true, Ordering::SeqCst);
    let h = harness_with(
        &url,
        "http://127.0.0.1:9/{z}/{x}/{y}.png",
        tempfile::tempdir().unwrap(),
        |c| c.dem_url = Some(dem_url),
    );
    let (status, _, body) = get_uri(&h.app, &area_uri()).await;
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
    assert_eq!(overpass.hits.load(Ordering::SeqCst), 0);
    // A tile that is a PNG but not a 256 x 256 Terrarium tile is not cached either.
    dem.fail.store(false, Ordering::SeqCst);
    dem.tiny.store(true, Ordering::SeqCst);
    let (status, _, body) = get_uri(&h.app, &area_uri()).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(String::from_utf8_lossy(&body).contains("unusable"));
    // Nothing was cached: the next request tries again.
    dem.tiny.store(false, Ordering::SeqCst);
    let (status, _, _) = get_uri(&h.app, &area_uri()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(overpass.hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn without_elevation_areas_are_flat() {
    let (_, url) = spawn_overpass().await;
    let h = harness(&url, "http://127.0.0.1:9/{z}/{x}/{y}.png");
    let (status, _, body) = get_uri(&h.app, &area_uri()).await;
    assert_eq!(status, StatusCode::OK);
    let b = AreaBundle::decode(&body).unwrap();
    assert!(b.terrain.is_flat() && b.terrain.attribution.is_empty());
    assert!(b.buildings.iter().all(|b| b.footprint.base_m == 0.0));
    // The demo keeps its recorded terrain either way.
    let (_, _, demo) = get_uri(&h.app, "/api/demo").await;
    assert!(!AreaBundle::decode(&demo).unwrap().terrain.is_flat());
}
