//! Ground elevation: a height grid over an area, sampled from Terrarium DEM
//! tiles (<https://github.com/tilezen/joerd/blob/master/docs/formats.md>).
//!
//! Heights are world `y` values: metres above the ground at the area origin
//! (the bbox centre), which is `y = 0`. [`Terrain::datum_m`] gives that
//! point's elevation above sea level. A [`Terrain::default`] grid is flat:
//! every height is 0.

mod dem;
mod grid;

pub use dem::{
    DEM_TILE_PX, DEM_ZOOM, DemTile, TERRAIN_TILES_ATTRIBUTION, TERRAIN_TILES_CREDITS_URL,
    dem_tiles_for, terrain_extent, terrain_from_dem, terrarium_m,
};
pub use grid::{
    FLAT, InvalidTerrain, MAX_TERRAIN_SAMPLES, TERRAIN_CELL_M, TERRAIN_MARGIN_M, Terrain,
};
