//! Messages between the client and its coverage worker.
//!
//! The client sends each newly loaded area once ([`JobRequest::LoadArea`]),
//! then a [`Job`] for every camera whose visibility needs recomputing; the
//! worker answers each with a [`JobResult`], or with
//! [`JobResponse::Stale`] if it belongs to another area. Every message is a
//! [`JOB_VERSION`] byte followed by postcard, so a worker script cached from
//! an older deployment is detected rather than misread.

use crate::building::Footprint;
use crate::bundle::{AreaBundle, Road};
use crate::coverage::{GroundGrid, StaticLayers, Visibility};
use crate::frustum::ViewCone;
use crate::terrain::Terrain;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const JOB_VERSION: u8 = 2;

/// Client to worker.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum JobRequest {
    /// Build the analysis layers for a new area. Jobs of other generations are
    /// ignored from then on.
    LoadArea {
        generation: u32,
        inputs: LayerInputs,
    },
    Compute(Job),
}

/// What [`StaticLayers::new`] takes from a bundle: the arguments of
/// [`StaticLayers::from_parts`], without the render mesh and addresses the
/// layers do not use. The worker builds its layers from these with the same
/// function, so they match the client's.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayerInputs {
    pub grid: GroundGrid,
    pub footprints: Vec<Footprint>,
    pub roads: Vec<Road>,
    pub facade_cell_m: f32,
    pub terrain: Terrain,
}

impl LayerInputs {
    pub fn new(bundle: &AreaBundle, ground_cell_m: f32, facade_cell_m: f32) -> Self {
        Self {
            grid: StaticLayers::grid_for(bundle, ground_cell_m),
            footprints: bundle.footprints().cloned().collect(),
            roads: bundle.roads.clone(),
            facade_cell_m,
            terrain: bundle.terrain.clone(),
        }
    }

    pub fn build(&self) -> StaticLayers {
        let footprints: Vec<&Footprint> = self.footprints.iter().collect();
        StaticLayers::from_parts(
            self.grid,
            &footprints,
            &self.roads,
            self.facade_cell_m,
            self.terrain.clone(),
        )
    }
}

/// One camera's visibility to compute.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Job {
    /// The area load this job belongs to.
    pub generation: u32,
    pub camera_id: u32,
    /// The client's cache key for the camera's inputs, echoed in the result.
    pub key: u64,
    pub cone: ViewCone,
    pub subject_height_m: f32,
}

impl Job {
    pub fn run(&self, layers: &StaticLayers) -> JobResult {
        JobResult {
            generation: self.generation,
            camera_id: self.camera_id,
            key: self.key,
            visibility: layers.visibility(&self.cone, self.subject_height_m),
        }
    }
}

/// A finished [`Job`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobResult {
    pub generation: u32,
    pub camera_id: u32,
    pub key: u64,
    pub visibility: Visibility,
}

/// Worker to client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum JobResponse {
    /// The worker is listening; messages posted before this are lost.
    Ready,
    Done(JobResult),
    /// A job the worker did not run because it holds a different area (or none).
    Stale {
        generation: u32,
        camera_id: u32,
        key: u64,
    },
    /// The worker could not use a request (an unreadable message).
    Failed(String),
}

#[derive(Debug, Error, PartialEq)]
pub enum JobError {
    #[error("coverage message version {0}, but this build speaks version {JOB_VERSION}")]
    Version(u8),
    #[error("unreadable coverage message ({0})")]
    Decode(String),
}

impl JobRequest {
    pub fn encode(&self) -> Vec<u8> {
        encode(self)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, JobError> {
        decode(bytes)
    }
}

impl JobResponse {
    pub fn encode(&self) -> Vec<u8> {
        encode(self)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, JobError> {
        decode(bytes)
    }
}

fn encode<T: Serialize>(msg: &T) -> Vec<u8> {
    // Serialising plain data into a Vec cannot fail.
    postcard::to_extend(msg, vec![JOB_VERSION]).unwrap_or_default()
}

fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, JobError> {
    match bytes.split_first() {
        Some((&JOB_VERSION, rest)) => {
            postcard::from_bytes(rest).map_err(|e| JobError::Decode(e.to_string()))
        }
        Some((&v, _)) => Err(JobError::Version(v)),
        None => Err(JobError::Decode("empty message".to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::{Vec2, Vec3};

    fn job() -> Job {
        Job {
            generation: 3,
            camera_id: 7,
            key: 0xdead_beef_0123_4567,
            cone: ViewCone::from_angles(
                Vec3::new(0.0, 3.0, 10.0),
                0.0,
                -10.0,
                0.0,
                90.0,
                60.0,
                30.0,
            ),
            subject_height_m: 1.0,
        }
    }

    #[test]
    fn round_trip() {
        let load = JobRequest::LoadArea {
            generation: 3,
            inputs: LayerInputs::new(&crate::bundle::tests::sample(), 0.5, 1.0),
        };
        let compute = JobRequest::Compute(job());
        for req in [load, compute] {
            let bytes = req.encode();
            assert_eq!(bytes[0], JOB_VERSION);
            assert_eq!(JobRequest::decode(&bytes), Ok(req));
        }
        let done = JobResponse::Done(JobResult {
            generation: 3,
            camera_id: 7,
            key: 42,
            visibility: Visibility {
                ground: vec![0, 5, 70_000],
                facade: vec![2],
            },
        });
        let stale = JobResponse::Stale {
            generation: 2,
            camera_id: 7,
            key: 42,
        };
        for resp in [
            JobResponse::Ready,
            done,
            stale,
            JobResponse::Failed("no".into()),
        ] {
            assert_eq!(JobResponse::decode(&resp.encode()), Ok(resp));
        }
    }

    #[test]
    fn rejects_garbage() {
        assert!(matches!(JobRequest::decode(&[]), Err(JobError::Decode(_))));
        let mut bytes = JobRequest::Compute(job()).encode();
        bytes[0] = 99;
        assert_eq!(JobRequest::decode(&bytes), Err(JobError::Version(99)));
        let mut bytes = JobRequest::Compute(job()).encode();
        bytes.truncate(bytes.len() / 2);
        assert!(matches!(
            JobRequest::decode(&bytes),
            Err(JobError::Decode(_))
        ));
        assert!(matches!(
            JobResponse::decode(&[JOB_VERSION, 0xff, 0xff, 0xff, 0xff, 0x0f]),
            Err(JobError::Decode(_))
        ));
    }

    #[test]
    fn run_matches_direct_computation() {
        let wall = Footprint {
            outer: vec![[-5.0, -5.0], [5.0, -5.0], [5.0, 5.0], [-5.0, 5.0]],
            holes: vec![],
            min_height_m: 0.0,
            height_m: 6.0,
            base_m: 0.0,
        };
        let grid = GroundGrid::covering(Vec2::new(-40.0, -40.0), Vec2::new(40.0, 40.0), 1.0);
        let layers = StaticLayers::from_parts(grid, &[&wall], &[], 1.0, Terrain::default());
        let j = job();
        let r = j.run(&layers);
        assert_eq!((r.generation, r.camera_id, r.key), (3, 7, j.key));
        assert_eq!(r.visibility, layers.visibility(&j.cone, 1.0));
        assert!(!r.visibility.ground.is_empty() && !r.visibility.facade.is_empty());
    }

    #[test]
    fn layer_inputs_build_the_same_layers() {
        let bundle = crate::bundle::tests::sample_on(crate::terrain::tests::slope());
        let direct = StaticLayers::new(&bundle, 0.5, 1.0);
        let inputs = LayerInputs::new(&bundle, 0.5, 1.0);
        let built = JobRequest::decode(
            &JobRequest::LoadArea {
                generation: 1,
                inputs,
            }
            .encode(),
        );
        let Ok(JobRequest::LoadArea { inputs, .. }) = built else {
            panic!("LoadArea did not round-trip: {built:?}");
        };
        let built = inputs.build();
        assert_eq!(built.grid, direct.grid);
        assert_eq!(built.building_mask, direct.building_mask);
        assert_eq!(built.street_mask, direct.street_mask);
        assert_eq!(built.facades, direct.facades);
        assert_eq!(built.terrain, direct.terrain);
        assert!(!built.terrain.is_flat());
        assert!(direct.building_mask.contains(&true) && direct.street_mask.contains(&true));
    }
}
