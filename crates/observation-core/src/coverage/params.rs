//! The user's analysis settings.

use crate::camera::{CameraModel, DoriLevel, Lighting, MAX_CUSTOM_PPM, MIN_CUSTOM_PPM};
use serde::{Deserialize, Serialize};

/// User-adjustable analysis settings.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CoverageParams {
    pub dori: DoriLevel,
    pub lighting: Lighting,
    /// Height above ground at which people are tested, metres
    /// (0 = ground, ~1.0 = torso, ~1.6 = face).
    pub subject_height_m: f32,
    /// Pixel density used when `dori` is [`DoriLevel::Custom`], px/m.
    #[serde(default = "default_custom_ppm")]
    pub custom_ppm: f32,
}

fn default_custom_ppm() -> f32 {
    250.0
}

impl CoverageParams {
    /// Pixel density the analysis requires, px/m.
    pub fn pixels_per_metre(&self) -> f32 {
        self.dori
            .pixels_per_metre()
            .unwrap_or_else(|| self.custom_ppm.clamp(MIN_CUSTOM_PPM, MAX_CUSTOM_PPM))
    }

    /// The detail levels the figures are given at, with their densities: the
    /// four standard ones, then Custom when it is the selected level.
    pub fn ladder(&self) -> Vec<(DoriLevel, f32)> {
        DoriLevel::ALL
            .iter()
            .filter(|l| **l != DoriLevel::Custom || self.dori == DoriLevel::Custom)
            .map(|&l| {
                let ppm = l
                    .pixels_per_metre()
                    .unwrap_or_else(|| self.pixels_per_metre());
                (l, ppm)
            })
            .collect()
    }

    /// How far a camera's sight lines must reach
    /// ([`StaticLayers::sight`](super::StaticLayers::sight)), metres: its
    /// longest range over the [`Self::ladder`] levels, which is by day (night
    /// only ever shortens it), at the lowest density.
    pub fn sight_range_m(&self, model: &CameraModel) -> f32 {
        model.effective_range_m(self.lowest_ppm(), Lighting::Day)
    }

    /// How far to compute a camera's sight lines when the ones it has fall
    /// short of [`Self::sight_range_m`], metres: that range, or, for a Custom
    /// density below Detect's, the range at half that density, so lowering it
    /// a little at a time recomputes once per halving instead of at every step.
    pub fn sight_compute_range_m(&self, model: &CameraModel) -> f32 {
        let ppm = self.lowest_ppm();
        let detect = DoriLevel::Detect.pixels_per_metre().unwrap_or(ppm);
        let ppm = if ppm < detect { ppm / 2.0 } else { ppm };
        model.effective_range_m(ppm, Lighting::Day)
    }

    /// The lowest density over the [`Self::ladder`] levels, px/m.
    fn lowest_ppm(&self) -> f32 {
        DoriLevel::ALL
            .iter()
            .filter_map(|l| l.pixels_per_metre())
            .fold(self.pixels_per_metre(), f32::min)
    }
}

impl Default for CoverageParams {
    fn default() -> Self {
        Self {
            dori: DoriLevel::Detect,
            lighting: Lighting::Day,
            subject_height_m: 1.0,
            custom_ppm: default_custom_ppm(),
        }
    }
}
