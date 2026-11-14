//! Camera models (catalog entries), placements and effective range.

mod catalog;
mod dori;
mod kind;
mod model;
mod placement;

pub use crate::util::url::is_https_url;
pub use catalog::{CUSTOM_ID_PREFIX, Catalog, CatalogError, fresh_custom_id};
pub use dori::{DoriLevel, Lighting, MAX_CUSTOM_PPM, MIN_CUSTOM_PPM};
pub use kind::CameraKind;
pub use model::{ASSUMED_IR_RANGE_M, CameraModel, MAX_FOV_DEG, MAX_RANGE_M};
pub use placement::{
    CameraPlacement, MAX_GROUP_CHARS, forward_from_yaw_pitch, yaw_pitch_from_forward,
};

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn model() -> CameraModel {
        CameraModel {
            id: "test-cam".into(),
            vendor: "Acme".into(),
            name: "Cam".into(),
            kind: CameraKind::Bullet,
            hfov_deg: 90.0,
            vfov_deg: 50.0,
            resolution: [1920, 1080],
            ir_range_m: Some(10.0),
            source: None,
            notes: None,
        }
    }
}
