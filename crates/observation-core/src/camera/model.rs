//! Camera models and their effective range.

use super::{CameraKind, CatalogError, Lighting, MIN_CUSTOM_PPM};
use crate::util::url::is_https_url;
use serde::{Deserialize, Serialize};

/// Widest field of view we model with a rectilinear projection. Fisheye
/// doorbells advertising more than this are clamped (documented limitation).
pub const MAX_FOV_DEG: f32 = 170.0;

/// Hard cap on modelled range, metres.
pub const MAX_RANGE_M: f32 = 100.0;

/// Night-vision range assumed when a manufacturer publishes none, metres
/// (≈25 ft, typical of consumer IR LEDs). The UI flags this as an assumption.
pub const ASSUMED_IR_RANGE_M: f32 = 7.5;

/// A camera product (or a user-defined custom model).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CameraModel {
    /// Stable kebab-case id, e.g. `"ring-battery-doorbell-plus"`.
    pub id: String,
    pub vendor: String,
    pub name: String,
    pub kind: CameraKind,
    /// Advertised horizontal field of view, degrees.
    pub hfov_deg: f32,
    /// Advertised vertical field of view, degrees.
    pub vfov_deg: f32,
    /// Sensor/stream resolution `[width, height]` in pixels.
    pub resolution: [u32; 2],
    /// Advertised infrared/night-vision range, metres (if any).
    #[serde(default)]
    pub ir_range_m: Option<f32>,
    /// Where the specs come from (manufacturer spec page).
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
}

impl CameraModel {
    pub fn display_name(&self) -> String {
        if self.vendor.is_empty() {
            self.name.clone()
        } else {
            format!("{} {}", self.vendor, self.name)
        }
    }

    pub fn hfov_clamped(&self) -> f32 {
        self.hfov_deg.clamp(1.0, MAX_FOV_DEG)
    }

    pub fn vfov_clamped(&self) -> f32 {
        self.vfov_deg.clamp(1.0, MAX_FOV_DEG)
    }

    /// Distance at which the horizontal pixel density drops to `ppm` pixels
    /// per metre, using a rectilinear approximation (centre of frame).
    pub fn dori_range_m(&self, ppm: f32) -> f32 {
        let half = (self.hfov_clamped() / 2.0).to_radians();
        let width_px = self.resolution[0].max(1) as f32;
        (width_px / (2.0 * ppm.max(MIN_CUSTOM_PPM) * half.tan())).min(MAX_RANGE_M)
    }

    /// Effective modelled range under the given conditions.
    pub fn effective_range_m(&self, ppm: f32, lighting: Lighting) -> f32 {
        let dori = self.dori_range_m(ppm);
        match lighting {
            Lighting::Day => dori,
            Lighting::Night => dori.min(self.ir_range_m.unwrap_or(ASSUMED_IR_RANGE_M)),
        }
    }

    /// Basic sanity checks for catalog and custom entries.
    pub fn validate(&self) -> Result<(), CatalogError> {
        let bad = |why: String| {
            Err(CatalogError::Invalid {
                id: self.id.clone(),
                why,
            })
        };
        if self.id.is_empty()
            || !self
                .id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return bad("the id must be lowercase letters, digits and dashes".to_owned());
        }
        for (what, fov) in [("horizontal", self.hfov_deg), ("vertical", self.vfov_deg)] {
            if !(1.0..=360.0).contains(&fov) {
                return bad(format!(
                    "{what} field of view is {fov}°, but must be 1° to 360°"
                ));
            }
        }
        if self.resolution[0] < 16 || self.resolution[1] < 16 {
            let [w, h] = self.resolution;
            return bad(format!(
                "resolution {w} × {h} is too small; each side needs at least 16 px"
            ));
        }
        if let Some(r) = self.ir_range_m.filter(|r| !(0.0..=200.0).contains(r)) {
            return bad(format!("IR range is {r} m, but must be 0 to 200 m"));
        }
        // The UI opens `source` as a link, and imported scene files carry their
        // own custom models: anything but a plain https URL (`javascript:`,
        // `data:`, ...) would run in or navigate away from the app's origin.
        if self.source.as_deref().is_some_and(|s| !is_https_url(s)) {
            return bad(
                "the source link must be an https:// URL without spaces or a user name".to_owned(),
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::model;
    use super::*;

    #[test]
    fn dori_ranges() {
        let m = model();
        // 1920 / (2 * 25 * tan 45°) = 38.4 m
        assert!((m.dori_range_m(25.0) - 38.4).abs() < 0.01);
        assert!(m.dori_range_m(250.0) < m.dori_range_m(125.0));
        assert_eq!(m.effective_range_m(25.0, Lighting::Night), 10.0);
        // A zero or negative density cannot yield an infinite or negative range
        // (the 100 m cap alone would hide an infinite one, so test a negative).
        assert_eq!(m.dori_range_m(0.0), m.dori_range_m(MIN_CUSTOM_PPM));
        assert_eq!(m.dori_range_m(-25.0), m.dori_range_m(MIN_CUSTOM_PPM));
    }

    #[test]
    fn source_must_be_https() {
        let with = |s: &str| CameraModel {
            source: Some(s.to_owned()),
            ..model()
        };
        assert!(with("https://example.com/spec.pdf").validate().is_ok());
        assert!(with("HTTPS://example.com").validate().is_ok());
        assert!(with("https://example.com:8443/a?b=c#d").validate().is_ok());
        for bad in [
            "javascript:alert(document.domain)",
            "JavaScript:alert(1)",
            " https://example.com",
            "https://exa mple.com",
            "https://example.com/\njavascript:x",
            "data:text/html,<script>alert(1)</script>",
            "http://example.com",
            "//example.com",
            "https://",
            "https:///path",
            "HTTPS:///path",
            "https://?q",
            "https://#f",
            "https://:443/",
            "https://\\evil.example",
            "https://ring.com@evil.example/spec",
            "https://@evil.example",
            "",
        ] {
            assert!(with(bad).validate().is_err(), "{bad:?}");
        }
        assert!(
            with(&format!("https://e.com/{}", "a".repeat(3000)))
                .validate()
                .is_err()
        );
    }
}
