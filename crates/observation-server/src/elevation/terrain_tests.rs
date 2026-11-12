//! Elevation on a fake upstream, a memory store and a manual clock.

use std::sync::Arc;
use std::time::Duration;

use axum::http::{Method, StatusCode, header};
use observation_core::geo::BBox;
use observation_core::terrain::{Terrain, dem_tiles_for};
use observation_core::tiles::TileId;
use observation_upstream::testing::{FakeHttp, FakeReply};

use super::Elevation;
use crate::clock::ManualClock;
use crate::config::Config;
use crate::error::ApiError;
use crate::ports::{TileMeta, TileSet, TileStore};
use crate::testing::{MemTileStore, MetricCounts};

const T: u64 = 1_700_000_000;
const TEMPLATE: &str = "https://dem.example/terrarium/{z}/{x}/{y}.png";

fn bbox() -> BBox {
    BBox::parse(include_str!("../../fixtures/demo-bbox.txt").trim()).unwrap()
}

fn url(id: TileId) -> String {
    id.url(TEMPLATE)
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

/// A valid 1 x 1 PNG.
const TINY_PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE,
    0x42, 0x60, 0x82,
];

struct Rig {
    elevation: Elevation,
    fake: FakeHttp,
    store: Arc<MemTileStore>,
}

/// The production configuration (30-day floor) with the DEM on a fake host.
fn rig(dem_url: Option<&str>) -> Rig {
    let mut config = Config::from_lookup(|_| None).unwrap();
    config.dem_url = dem_url.map(str::to_owned);
    config.dem_attribution = "Elevation: test".into();
    let fake = FakeHttp::new();
    let store = Arc::new(MemTileStore::new());
    let elevation = Elevation::new(
        Arc::new(fake.clone()),
        &config,
        store.clone(),
        Arc::new(ManualClock::at_unix(T)),
    );
    Rig {
        elevation,
        fake,
        store,
    }
}

/// Replies for every tile `bbox` needs, the later ones faster, so they
/// finish in a different order from the requests.
fn script_tiles(fake: &FakeHttp) -> Vec<TileId> {
    let ids = dem_tiles_for(&bbox());
    for (i, id) in ids.iter().enumerate() {
        let delay = Duration::from_millis(100 * (ids.len() - i) as u64);
        fake.on(
            Method::GET,
            &url(*id),
            FakeReply::ok(dem_png(id.y))
                .header(header::CACHE_CONTROL, "max-age=60")
                .delay(delay),
        );
    }
    ids
}

#[tokio::test(start_paused = true)]
async fn terrain_is_built_from_every_tile_once_then_from_the_store() {
    let recorder = MetricCounts::default();
    let _guard = metrics::set_default_local_recorder(&recorder);
    let r = rig(Some(TEMPLATE));
    let ids = script_tiles(&r.fake);
    assert!(ids.len() >= 2, "the demo box spans tiles");
    let terrain = r.elevation.terrain(&bbox()).await.unwrap();
    assert!(!terrain.is_flat());
    assert_eq!(terrain.attribution, "Elevation: test");
    r.fake.assert_drained();
    for req in r.fake.requests() {
        assert_eq!(req.method, Method::GET);
        assert!(
            req.headers().get(header::REFERER).is_none(),
            "never for elevation"
        );
        assert_eq!(req.timeout, None);
        assert_eq!(req.error_max_bytes, 0);
    }
    for id in &ids {
        let stored = r.store.get(TileSet::Dem, *id).await.unwrap();
        // The 30-day floor over max-age=60.
        assert_eq!(stored.meta.expires_unix, T + 30 * 86_400);
        assert!(r.store.get(TileSet::Map, *id).await.is_none());
    }
    let n = ids.len() as u64;
    assert_eq!(
        recorder.counters(crate::metrics::DEM_CACHE),
        [("result=miss".to_owned(), n)]
    );

    // Again: from the store, one hit per tile, and the same terrain.
    let again = r.elevation.terrain(&bbox()).await.unwrap();
    assert_eq!(again, terrain);
    assert_eq!(r.fake.requests().len(), ids.len());
    assert_eq!(
        recorder.counters(crate::metrics::DEM_CACHE),
        [("result=hit".to_owned(), n), ("result=miss".to_owned(), n)]
    );
    assert!(recorder.counters(crate::metrics::TILE_CACHE).is_empty());
}

#[tokio::test(start_paused = true)]
async fn one_failed_tile_fails_the_terrain_and_is_not_stored() {
    for (reply, message) in [
        (
            FakeReply::status(StatusCode::INTERNAL_SERVER_ERROR),
            "the elevation server answered with an error (HTTP 500)",
        ),
        (
            FakeReply::status(StatusCode::NOT_FOUND),
            "the elevation server has no tile here",
        ),
        (
            // A PNG, but not a 256 x 256 Terrarium tile.
            FakeReply::ok(TINY_PNG),
            "the elevation server sent an unusable tile",
        ),
    ] {
        let r = rig(Some(TEMPLATE));
        let ids = dem_tiles_for(&bbox());
        r.fake.on(Method::GET, &url(ids[0]), reply.clone());
        for id in &ids[1..] {
            r.fake
                .on(Method::GET, &url(*id), FakeReply::ok(dem_png(id.y)));
        }
        let e = r.elevation.terrain(&bbox()).await.unwrap_err();
        assert!(matches!(&e, ApiError::Upstream(m) if m == message), "{e:?}");
        assert_eq!(e.status(), StatusCode::BAD_GATEWAY);
        assert!(r.store.get(TileSet::Dem, ids[0]).await.is_none());
    }
}

#[tokio::test(start_paused = true)]
async fn an_expired_tile_is_served_when_the_upstream_fails() {
    let recorder = MetricCounts::default();
    let _guard = metrics::set_default_local_recorder(&recorder);
    let r = rig(Some(TEMPLATE));
    let ids = dem_tiles_for(&bbox());
    let expired = TileMeta {
        expires_unix: T,
        ..TileMeta::default()
    };
    for id in &ids {
        r.store
            .put(TileSet::Dem, *id, &dem_png(id.y), &expired)
            .await
            .unwrap();
        r.fake.on(
            Method::GET,
            &url(*id),
            FakeReply::status(StatusCode::BAD_GATEWAY),
        );
    }
    assert!(!r.elevation.terrain(&bbox()).await.unwrap().is_flat());
    assert_eq!(
        recorder.counters(crate::metrics::DEM_CACHE),
        [("result=stale_served".to_owned(), ids.len() as u64)]
    );
}

#[tokio::test]
async fn without_elevation_areas_are_flat_and_nothing_is_asked() {
    let r = rig(None);
    assert_eq!(
        r.elevation.terrain(&bbox()).await.unwrap(),
        Terrain::default()
    );
    assert!(r.fake.requests().is_empty());
}

/// A flat Terrarium tile at `meters`.
fn level_png(meters: u32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, 256, 256);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header().unwrap();
    let e = 32768 + meters;
    let data: Vec<u8> = (0..256 * 256)
        .flat_map(|_| [(e >> 8) as u8, (e & 0xff) as u8, 0])
        .collect();
    w.write_image_data(&data).unwrap();
    w.finish().unwrap();
    out
}

/// Each tile's bytes are decoded as that tile: with a different height per
/// tile, the terrain equals one built directly from the same tiles.
#[tokio::test(start_paused = true)]
async fn each_tile_is_decoded_as_its_own_id() {
    let r = rig(Some(TEMPLATE));
    let ids = dem_tiles_for(&bbox());
    assert!(ids.len() >= 2, "the demo box spans tiles");
    let height = |i: usize| 100 + 37 * i as u32;
    for (i, id) in ids.iter().enumerate() {
        // Later tiles answer first, so completion order differs from `ids`.
        let delay = Duration::from_millis(100 * (ids.len() - i) as u64);
        r.fake.on(
            Method::GET,
            &url(*id),
            FakeReply::ok(level_png(height(i))).delay(delay),
        );
    }
    let terrain = r.elevation.terrain(&bbox()).await.unwrap();
    let tiles: Vec<_> = ids
        .iter()
        .enumerate()
        .map(|(i, id)| super::decode_tile(*id, &level_png(height(i))).unwrap())
        .collect();
    let want =
        observation_core::terrain::terrain_from_dem(&bbox(), &tiles, "Elevation: test".to_owned())
            .unwrap();
    assert_eq!(terrain, want);
    r.fake.assert_drained();
}
