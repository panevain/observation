//! Ground heights for bundles: Terrarium elevation tiles fetched through a
//! [`TileProxy`] (cached under `dem/`), decoded and sampled into a
//! [`Terrain`] grid.

use std::sync::Arc;

use observation_core::geo::BBox;
use observation_core::terrain::{DEM_TILE_PX, DemTile, Terrain, dem_tiles_for, terrain_from_dem};
use observation_core::tiles::TileId;

use crate::cache::Cache;
use crate::config::Config;
use crate::error::ApiError;
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
    pub fn new(client: reqwest::Client, config: &Config, cache: Arc<Cache>) -> Self {
        Self {
            proxy: config
                .dem_url
                .clone()
                .map(|t| TileProxy::elevation(client, t, is_terrarium, config, cache)),
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
            futures_util::future::join_all(ids.iter().map(|&id| proxy.get(id, None))).await;
        let mut pngs = Vec::with_capacity(ids.len());
        for (id, png) in ids.into_iter().zip(fetched) {
            // A missing tile is the elevation server's failure, not a missing area.
            let png = png.map_err(|e| match e {
                ApiError::NotFound(why) => ApiError::Upstream(why.to_owned()),
                other => other,
            })?;
            pngs.push((id, png));
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

/// Decodes one Terrarium PNG.
pub fn decode_tile(id: TileId, png: &[u8]) -> Result<DemTile, ApiError> {
    let bad = |why: &str| {
        tracing::warn!(?id, why, "unusable elevation tile");
        ApiError::Upstream("the elevation server sent an unusable tile".into())
    };
    let rgb = terrarium_rgb(png).ok_or_else(|| bad("not a 256 × 256 RGB PNG"))?;
    DemTile::from_terrarium_rgb(id, &rgb).ok_or_else(|| bad("wrong size"))
}

/// Whether `png` is a Terrarium tile [`decode_tile`] can use.
fn is_terrarium(png: &[u8]) -> bool {
    terrarium_rgb(png).is_some()
}

/// 8-bit RGB pixels of a [`DEM_TILE_PX`]² PNG (RGBA and palette images are
/// converted), or `None` for any other image.
fn terrarium_rgb(png: &[u8]) -> Option<Vec<u8>> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(png));
    // Palette and sub-byte images become 8-bit; 16-bit is cut to 8.
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().ok()?;
    let info = reader.info();
    if info.width != DEM_TILE_PX || info.height != DEM_TILE_PX {
        return None;
    }
    let mut buf = vec![0; reader.output_buffer_size()?];
    let frame = reader.next_frame(&mut buf).ok()?;
    let pixels = &buf[..frame.buffer_size()];
    match (frame.color_type, frame.bit_depth) {
        (png::ColorType::Rgb, png::BitDepth::Eight) => Some(pixels.to_vec()),
        (png::ColorType::Rgba, png::BitDepth::Eight) => Some(
            pixels
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|p| [p[0], p[1], p[2]])
                .collect(),
        ),
        _ => None,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A Terrarium PNG with every pixel at `elevation_m` (whole metres).
    pub(crate) fn flat_png(elevation_m: u32, alpha: bool) -> Vec<u8> {
        let e = 32768 + elevation_m;
        let (r, g) = ((e >> 8) as u8, (e & 0xff) as u8);
        let mut out = Vec::new();
        let mut enc = png::Encoder::new(&mut out, DEM_TILE_PX, DEM_TILE_PX);
        enc.set_color(if alpha {
            png::ColorType::Rgba
        } else {
            png::ColorType::Rgb
        });
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header().unwrap();
        let px: &[u8] = if alpha { &[r, g, 0, 255] } else { &[r, g, 0] };
        let data: Vec<u8> = px
            .iter()
            .copied()
            .cycle()
            .take(px.len() * (DEM_TILE_PX * DEM_TILE_PX) as usize)
            .collect();
        w.write_image_data(&data).unwrap();
        w.finish().unwrap();
        out
    }

    #[test]
    fn decodes_terrarium_pngs() {
        let id = TileId::new(15, 1, 2).unwrap();
        for alpha in [false, true] {
            let t = decode_tile(id, &flat_png(312, alpha)).unwrap();
            assert_eq!(t.id, id);
            assert!(t.elevations.iter().all(|&e| e == 312.0));
        }
        assert!(decode_tile(id, b"\x89PNG\r\n\x1a\nnope").is_err());
        // The wrong size.
        let mut small = Vec::new();
        let mut enc = png::Encoder::new(&mut small, 2, 2);
        enc.set_color(png::ColorType::Rgb);
        let mut w = enc.write_header().unwrap();
        w.write_image_data(&[0; 12]).unwrap();
        w.finish().unwrap();
        assert!(decode_tile(id, &small).is_err());
    }
}
