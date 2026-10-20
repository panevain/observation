//! `/api/tiles/{z}/{x}/{y}.png`.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use observation_core::tiles::TileId;

use crate::AppState;
use crate::error::ApiError;

/// Parses the path segments into a valid [`TileId`]; anything else is a 404.
fn parse_tile(z: &str, x: &str, y: &str) -> Option<TileId> {
    let y = y.strip_suffix(".png")?;
    TileId::new(z.parse().ok()?, x.parse().ok()?, y.parse().ok()?)
}

/// Caching proxy for map tiles.
pub async fn tile(
    State(state): State<AppState>,
    Path((z, x, y)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let id = parse_tile(&z, &x, &y).ok_or(ApiError::NotFound("not a valid map tile address"))?;
    let bytes = state.tiles().get(id, headers.get(header::REFERER)).await?;
    Ok((
        [
            (header::CONTENT_TYPE, HeaderValue::from_static("image/png")),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=604800"),
            ),
        ],
        bytes,
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tile_path_validation() {
        assert!(parse_tile("17", "10", "20.png").is_some());
        for (z, x, y) in [
            ("20", "0", "0.png"),
            ("2", "4", "0.png"),
            ("2", "0", "4.png"),
            ("2", "0", "0"),
            ("2", "-1", "0.png"),
            ("a", "0", "0.png"),
            ("2", "0", "0.jpg"),
            ("2", "0", "..png"),
        ] {
            assert!(parse_tile(z, x, y).is_none(), "{z}/{x}/{y}");
        }
    }
}
