//! Decoding Terrarium elevation tiles (PNG, height in the red, green and
//! blue channels).

use observation_core::terrain::{DEM_TILE_PX, DemTile};
use observation_core::tiles::TileId;
use thiserror::Error;

/// Why a PNG is not a usable Terrarium tile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum TileError {
    #[error("not a 256 × 256 RGB PNG")]
    NotTerrarium,
    #[error("wrong size")]
    WrongSize,
}

/// Decodes one Terrarium PNG as tile `id`.
pub fn decode(id: TileId, png: &[u8]) -> Result<DemTile, TileError> {
    let rgb = rgb(png).ok_or(TileError::NotTerrarium)?;
    DemTile::from_terrarium_rgb(id, &rgb).ok_or(TileError::WrongSize)
}

/// Whether `png` is a Terrarium tile [`decode`] can use.
pub fn is_terrarium(png: &[u8]) -> bool {
    rgb(png).is_some()
}

/// 8-bit RGB pixels of a [`DEM_TILE_PX`]² PNG (RGBA and palette images are
/// converted), or `None` for any other image.
fn rgb(png: &[u8]) -> Option<Vec<u8>> {
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
mod tests {
    use super::*;

    /// A Terrarium PNG with every pixel at `elevation_m` (whole metres).
    fn flat_png(elevation_m: u32, alpha: bool) -> Vec<u8> {
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
            let png = flat_png(312, alpha);
            assert!(is_terrarium(&png));
            let t = decode(id, &png).unwrap();
            assert_eq!(t.id, id);
            assert!(t.elevations.iter().all(|&e| e == 312.0));
        }
        assert_eq!(
            decode(id, b"\x89PNG\r\n\x1a\nnope").unwrap_err(),
            TileError::NotTerrarium
        );
        // The wrong size.
        let mut small = Vec::new();
        let mut enc = png::Encoder::new(&mut small, 2, 2);
        enc.set_color(png::ColorType::Rgb);
        let mut w = enc.write_header().unwrap();
        w.write_image_data(&[0; 12]).unwrap();
        w.finish().unwrap();
        assert!(!is_terrarium(&small));
        assert!(decode(id, &small).is_err());
    }
}
