//! Mount archetypes.

use serde::{Deserialize, Serialize};

/// Mount archetype; drives default mount height and UI grouping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum CameraKind {
    Doorbell,
    Floodlight,
    Spotlight,
    #[default]
    Bullet,
    Turret,
    Dome,
    Indoor,
}

impl CameraKind {
    pub const ALL: [Self; 7] = [
        Self::Doorbell,
        Self::Floodlight,
        Self::Spotlight,
        Self::Bullet,
        Self::Turret,
        Self::Dome,
        Self::Indoor,
    ];

    /// Typical mount height above ground, metres.
    pub fn default_mount_height_m(self) -> f32 {
        match self {
            Self::Doorbell => 1.2,
            Self::Indoor => 2.0,
            Self::Floodlight | Self::Spotlight => 2.7,
            Self::Bullet | Self::Turret | Self::Dome => 2.6,
        }
    }

    /// Typical downward tilt when first placed, degrees (negative = down).
    pub fn default_pitch_deg(self) -> f32 {
        match self {
            Self::Doorbell => 0.0,
            Self::Indoor => -10.0,
            _ => -20.0,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Doorbell => "Doorbell",
            Self::Floodlight => "Floodlight",
            Self::Spotlight => "Spotlight",
            Self::Bullet => "Bullet",
            Self::Turret => "Turret",
            Self::Dome => "Dome",
            Self::Indoor => "Indoor",
        }
    }
}
