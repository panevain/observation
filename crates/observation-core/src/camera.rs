//! Camera models (catalog entries), placements and effective range.

use glam::Vec3;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Widest field of view we model with a rectilinear projection. Fisheye
/// doorbells advertising more than this are clamped (documented limitation).
pub const MAX_FOV_DEG: f32 = 170.0;

/// Hard cap on modelled range, metres.
pub const MAX_RANGE_M: f32 = 100.0;

/// Night-vision range assumed when a manufacturer publishes none, metres
/// (≈25 ft, typical of consumer IR LEDs). The UI flags this as an assumption.
pub const ASSUMED_IR_RANGE_M: f32 = 7.5;

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

    /// Distance at which the horizontal pixel density drops to the DORI
    /// threshold, using a rectilinear approximation (centre of frame).
    pub fn dori_range_m(&self, level: DoriLevel) -> f32 {
        let half = (self.hfov_clamped() / 2.0).to_radians();
        let width_px = self.resolution[0].max(1) as f32;
        (width_px / (2.0 * level.pixels_per_metre() * half.tan())).min(MAX_RANGE_M)
    }

    /// Effective modelled range under the given conditions.
    pub fn effective_range_m(&self, level: DoriLevel, lighting: Lighting) -> f32 {
        let dori = self.dori_range_m(level);
        match lighting {
            Lighting::Day => dori,
            Lighting::Night => dori.min(self.ir_range_m.unwrap_or(ASSUMED_IR_RANGE_M)),
        }
    }

    /// Basic sanity checks for catalog and custom entries.
    pub fn validate(&self) -> Result<(), CatalogError> {
        let bad = |why: &str| {
            Err(CatalogError::Invalid {
                id: self.id.clone(),
                why: why.to_owned(),
            })
        };
        if self.id.is_empty()
            || !self
                .id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return bad("id must be non-empty kebab-case");
        }
        if !(1.0..=360.0).contains(&self.hfov_deg) || !(1.0..=360.0).contains(&self.vfov_deg) {
            return bad("fov out of range");
        }
        if self.resolution[0] < 16 || self.resolution[1] < 16 {
            return bad("resolution too small");
        }
        if self.ir_range_m.is_some_and(|r| !(0.0..=200.0).contains(&r)) {
            return bad("ir range out of range");
        }
        // The UI opens `source` as a link, and imported scene files carry their
        // own custom models: anything but a plain https URL (`javascript:`,
        // `data:`, ...) would run in or navigate away from the app's origin.
        if self.source.as_deref().is_some_and(|s| !is_https_url(s)) {
            return bad("source must be an https:// URL");
        }
        Ok(())
    }
}

/// Longest `source` URL accepted.
const MAX_SOURCE_LEN: usize = 2048;

/// An absolute `https://` URL with a non-empty host, no user info (so
/// `https://trusted.example@evil.example` cannot pose as another site) and no
/// whitespace or control characters.
pub fn is_https_url(s: &str) -> bool {
    if s.len() > MAX_SOURCE_LEN || s.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return false;
    }
    let Some((scheme, rest)) = s.split_at_checked(8) else {
        return false;
    };
    // Browsers end the authority at `/`, `\`, `?` or `#`.
    let authority = rest.split(['/', '\\', '?', '#']).next().unwrap_or_default();
    scheme.eq_ignore_ascii_case("https://")
        && !authority.is_empty()
        && !authority.starts_with(':')
        && !authority.contains('@')
}

/// DORI (EN 62676-4) pixel-density thresholds.
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
}

impl DoriLevel {
    pub const ALL: [Self; 4] = [Self::Detect, Self::Observe, Self::Recognise, Self::Identify];

    pub fn pixels_per_metre(self) -> f32 {
        match self {
            Self::Detect => 25.0,
            Self::Observe => 62.0,
            Self::Recognise => 125.0,
            Self::Identify => 250.0,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Detect => "Detect",
            Self::Observe => "Observe",
            Self::Recognise => "Recognise",
            Self::Identify => "Identify",
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

/// Where a camera sits and where it points.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CameraPlacement {
    pub id: u32,
    pub label: String,
    pub model_id: String,
    /// World-frame position, metres.
    pub position: [f32; 3],
    /// Compass bearing of the optical axis, degrees clockwise from north.
    pub yaw_deg: f32,
    /// Elevation of the optical axis, degrees (negative = looking down).
    pub pitch_deg: f32,
    /// Rotation about the optical axis, degrees.
    #[serde(default)]
    pub roll_deg: f32,
    /// Colour of the camera, its view cone and its per-camera overlay (sRGB).
    /// `None` uses the default colour for the model's [`CameraKind`]. Files
    /// from before this field carried a `color_index`, which is ignored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<[u8; 3]>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

impl CameraPlacement {
    pub fn eye(&self) -> Vec3 {
        Vec3::from_array(self.position)
    }

    pub fn forward(&self) -> Vec3 {
        forward_from_yaw_pitch(self.yaw_deg, self.pitch_deg)
    }

    /// Points the camera at `target` (keeps roll).
    pub fn look_at(&mut self, target: Vec3) {
        if let Some((yaw, pitch)) = yaw_pitch_from_forward(target - self.eye()) {
            self.yaw_deg = yaw;
            self.pitch_deg = pitch;
        }
    }
}

/// Unit forward vector for a compass bearing and elevation (degrees).
pub fn forward_from_yaw_pitch(yaw_deg: f32, pitch_deg: f32) -> Vec3 {
    let (sy, cy) = yaw_deg.to_radians().sin_cos();
    let (sp, cp) = pitch_deg.to_radians().sin_cos();
    Vec3::new(sy * cp, sp, -cy * cp)
}

/// Inverse of [`forward_from_yaw_pitch`]; yaw in `[0, 360)`. `None` for zero vectors.
pub fn yaw_pitch_from_forward(v: Vec3) -> Option<(f32, f32)> {
    let v = v.try_normalize()?;
    let yaw = v.x.atan2(-v.z).to_degrees().rem_euclid(360.0);
    let pitch = v.y.clamp(-1.0, 1.0).asin().to_degrees();
    Some((yaw, pitch))
}

#[derive(Debug, Error)]
pub enum CatalogError {
    #[error("catalog parse error: {0}")]
    Parse(String),
    #[error("invalid model `{id}`: {why}")]
    Invalid { id: String, why: String },
    #[error("duplicate model id `{0}`")]
    Duplicate(String),
}

/// The curated model list shipped with the app.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Catalog {
    pub version: u32,
    pub models: Vec<CameraModel>,
}

impl Catalog {
    /// Parses and validates a RON catalog.
    pub fn from_ron(s: &str) -> Result<Self, CatalogError> {
        let cat: Self = ron::from_str(s).map_err(|e| CatalogError::Parse(e.to_string()))?;
        let mut seen = std::collections::HashSet::new();
        for m in &cat.models {
            m.validate()?;
            if !seen.insert(m.id.as_str()) {
                return Err(CatalogError::Duplicate(m.id.clone()));
            }
        }
        Ok(cat)
    }

    /// Parses per-vendor RON files (each a bare list of models) into one validated
    /// catalog, ordered by kind, then vendor, then name.
    pub fn from_ron_parts(parts: &[&str]) -> Result<Self, CatalogError> {
        let mut models = Vec::new();
        for part in parts {
            let list: Vec<CameraModel> =
                ron::from_str(part).map_err(|e| CatalogError::Parse(e.to_string()))?;
            models.extend(list);
        }
        let kind_rank = |k: CameraKind| CameraKind::ALL.iter().position(|a| *a == k);
        models.sort_by(|a, b| {
            (kind_rank(a.kind), &a.vendor, &a.name).cmp(&(kind_rank(b.kind), &b.vendor, &b.name))
        });
        let mut seen = std::collections::HashSet::new();
        for m in &models {
            m.validate()?;
            if !seen.insert(m.id.as_str()) {
                return Err(CatalogError::Duplicate(m.id.clone()));
            }
        }
        Ok(Self { version: 1, models })
    }

    pub fn get(&self, id: &str) -> Option<&CameraModel> {
        self.models.iter().find(|m| m.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> CameraModel {
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

    #[test]
    fn dori_ranges() {
        let m = model();
        // 1920 / (2 * 25 * tan 45°) = 38.4 m
        assert!((m.dori_range_m(DoriLevel::Detect) - 38.4).abs() < 0.01);
        assert!(m.dori_range_m(DoriLevel::Identify) < m.dori_range_m(DoriLevel::Recognise));
        assert_eq!(
            m.effective_range_m(DoriLevel::Detect, Lighting::Night),
            10.0
        );
    }

    #[test]
    fn yaw_pitch_round_trip() {
        for (yaw, pitch) in [(0.0, 0.0), (90.0, -20.0), (225.0, 10.0), (359.0, -60.0)] {
            let f = forward_from_yaw_pitch(yaw, pitch);
            let (y2, p2) = yaw_pitch_from_forward(f).unwrap();
            assert!(
                (y2 - yaw).abs() < 1e-3 && (p2 - pitch).abs() < 1e-3,
                "{yaw},{pitch} -> {y2},{p2}"
            );
        }
        let north = forward_from_yaw_pitch(0.0, 0.0);
        assert!(north.z < -0.99);
        let east = forward_from_yaw_pitch(90.0, 0.0);
        assert!(east.x > 0.99);
    }

    #[test]
    fn catalog_parse_and_validate() {
        let ron = r#"(version: 1, models: [(id: "a-b", vendor: "V", name: "N", kind: Doorbell,
            hfov_deg: 150.0, vfov_deg: 150.0, resolution: (1536, 1536), ir_range_m: Some(5.0),
            source: Some("https://example.com"))])"#;
        let c = Catalog::from_ron(ron).unwrap();
        assert_eq!(c.get("a-b").unwrap().kind, CameraKind::Doorbell);
        let dup = ron.replace("models: [(", "models: [(id: \"a-b\", vendor: \"V\", name: \"N\", kind: Doorbell, hfov_deg: 150.0, vfov_deg: 150.0, resolution: (1536, 1536)), (");
        assert!(matches!(
            Catalog::from_ron(&dup),
            Err(CatalogError::Duplicate(_))
        ));
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

    #[test]
    fn look_at_points_camera() {
        let mut p = CameraPlacement {
            id: 1,
            label: "x".into(),
            model_id: "m".into(),
            position: [0.0, 2.0, 0.0],
            yaw_deg: 0.0,
            pitch_deg: 0.0,
            roll_deg: 0.0,
            color: None,
            enabled: true,
        };
        p.look_at(Vec3::new(10.0, 0.0, 0.0));
        assert!((p.yaw_deg - 90.0).abs() < 1e-3);
        assert!(p.pitch_deg < 0.0);
    }
}
