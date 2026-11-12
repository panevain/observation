//! Detail levels (DORI) and lighting.

use serde::{Deserialize, Serialize};

/// Allowed range for a user-chosen pixel density, px/m.
pub const MIN_CUSTOM_PPM: f32 = 1.0;
pub const MAX_CUSTOM_PPM: f32 = 2000.0;

/// DORI (EN 62676-4) pixel-density thresholds, plus a user-chosen density
/// (`CoverageParams::custom_ppm`) for calibrating against real footage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum DoriLevel {
    /// 25 px/m — can tell a person is present.
    #[default]
    Detect,
    /// 62 px/m — general activity / clothing.
    Observe,
    /// 125 px/m — recognise a known person.
    Recognise,
    /// 250 px/m — identify a stranger.
    Identify,
    /// User-chosen px/m.
    Custom,
}

impl DoriLevel {
    pub const ALL: [Self; 5] = [
        Self::Detect,
        Self::Observe,
        Self::Recognise,
        Self::Identify,
        Self::Custom,
    ];

    /// The standard density; `None` for [`Self::Custom`].
    pub fn pixels_per_metre(self) -> Option<f32> {
        match self {
            Self::Detect => Some(25.0),
            Self::Observe => Some(62.0),
            Self::Recognise => Some(125.0),
            Self::Identify => Some(250.0),
            Self::Custom => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Detect => "Detect",
            Self::Observe => "Observe",
            Self::Recognise => "Recognise",
            Self::Identify => "Identify",
            Self::Custom => "Custom",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum Lighting {
    #[default]
    Day,
    /// Limited to the camera's IR range.
    Night,
}
