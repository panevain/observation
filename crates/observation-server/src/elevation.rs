//! Ground heights for bundles: Terrarium elevation tiles fetched through a
//! [`TileProxy`] (cached under `dem/`), decoded and sampled into a
//! [`Terrain`] grid.

use std::sync::Arc;

use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use observation_core::geo::BBox;
use observation_core::terrain::{DemTile, Terrain, dem_tiles_for, terrain_from_dem};
use observation_core::tiles::TileId;
use observation_mapdata::terrarium;
use observation_upstream::HttpClient;

use crate::clock::Clock;
use crate::config::Config;
use crate::error::ApiError;
use crate::ports::{TerrainSource, TileStore};
use crate::tiles::TileProxy;

/// Where bundles get their terrain from.
#[derive(Debug)]
pub struct Elevation {
    /// `None` when `OBS_DEM_URL` is `off`: every area is flat.
    proxy: Option<TileProxy>,
    attribution: String,
}

impl Elevation {
    /// `client` must already carry the project `User-Agent` and a timeout.
    pub fn new(
        client: Arc<dyn HttpClient>,
        config: &Config,
        cache: Arc<dyn TileStore>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            proxy: config.dem_url.clone().map(|t| {
                TileProxy::elevation(client, t, terrarium::is_terrarium, config, cache, clock)
            }),
            attribution: config.dem_attribution.clone(),
        }
    }

    /// The terrain under `bbox` and its margin; flat if elevation is off.
    pub async fn terrain(&self, bbox: &BBox) -> Result<Terrain, ApiError> {
        let Some(proxy) = &self.proxy else {
            return Ok(Terrain::default());
        };
        let ids = dem_tiles_for(bbox);
        let fetched =
            futures_util::future::join_all(ids.iter().map(|&id| proxy.get(id, None, None))).await;
        let mut pngs = Vec::with_capacity(ids.len());
        for (id, png) in ids.into_iter().zip(fetched) {
            // A missing tile is the elevation server's failure, not a missing area.
            let tile = png.map_err(|e| match e {
                ApiError::NotFound(why) => ApiError::Upstream(why.to_owned()),
                other => other,
            })?;
            pngs.push((id, tile.bytes));
        }
        let (bbox, attribution) = (*bbox, self.attribution.clone());
        tokio::task::spawn_blocking(move || {
            let tiles = pngs
                .iter()
                .map(|(id, png)| decode_tile(*id, png))
                .collect::<Result<Vec<_>, _>>()?;
            terrain_from_dem(&bbox, &tiles, attribution).ok_or_else(|| {
                ApiError::Upstream("the elevation tiles did not cover the area".into())
            })
        })
        .await
        .map_err(|e| ApiError::Internal(format!("elevation task failed: {e}")))?
    }
}

impl TerrainSource for Elevation {
    fn terrain<'a>(&'a self, bbox: &'a BBox) -> BoxFuture<'a, Result<Terrain, ApiError>> {
        Elevation::terrain(self, bbox).boxed()
    }
}

/// Decodes one Terrarium PNG.
pub fn decode_tile(id: TileId, png: &[u8]) -> Result<DemTile, ApiError> {
    terrarium::decode(id, png).map_err(|why| {
        tracing::warn!(?id, %why, "unusable elevation tile");
        ApiError::Upstream("the elevation server sent an unusable tile".into())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unusable_tile_is_the_upstream_s_failure() {
        let id = TileId::new(15, 1, 2).unwrap();
        assert!(matches!(
            decode_tile(id, b"\x89PNG\r\n\x1a\nnope"),
            Err(ApiError::Upstream(_))
        ));
    }
}

#[cfg(test)]
mod terrain_tests;
