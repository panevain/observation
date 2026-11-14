use axum::Router;
use observation_server::config::Config;
use observation_server::{AppState, app};

pub(crate) struct Harness {
    pub(crate) app: Router,
    pub(crate) cache: tempfile::TempDir,
    _static: tempfile::TempDir,
}

pub(crate) fn harness_with(
    overpass_url: &str,
    tile_url: &str,
    static_dir: tempfile::TempDir,
    tweak: impl FnOnce(&mut Config),
) -> Harness {
    let cache = tempfile::tempdir().unwrap();
    let mut config = Config::for_tests(cache.path(), static_dir.path(), overpass_url, tile_url);
    tweak(&mut config);
    let app = app(AppState::new(config).unwrap());
    Harness {
        app,
        cache,
        _static: static_dir,
    }
}

pub(crate) fn harness(overpass_url: &str, tile_url: &str) -> Harness {
    harness_with(overpass_url, tile_url, tempfile::tempdir().unwrap(), |_| {})
}
