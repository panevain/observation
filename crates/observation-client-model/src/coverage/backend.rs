//! Where coverage jobs run: in a background worker when there is one, inline
//! otherwise.
//!
//! [`CoverageState`](super::CoverageState) decides what to compute and keeps
//! the results; this only runs jobs. In the browser they go to the
//! `coverage-worker` binary through a [`JobChannel`], so the frame never
//! waits for them. Natively, and whenever the worker cannot be started or
//! fails, they are computed inline on the main thread.

use std::collections::BTreeMap;
use std::sync::Arc;

use observation_core::bundle::AreaBundle;
use observation_core::coverage::StaticLayers;
use observation_core::coverage_job::{Job, JobRequest, JobResponse, JobResult, LayerInputs};
use observation_core::frustum::ViewCone;

use crate::platform::{Clock, JobChannel};

/// How long the worker may take to load and say it is ready, ms. A module
/// worker whose script loads but whose wasm does not (an HTML page in its
/// place, say) may never report an error.
pub const READY_WITHIN_MS: f64 = 10_000.0;

/// Runs coverage jobs. Not `Send` with a worker: the client keeps it in a
/// non-send resource.
#[derive(Debug, Default)]
pub struct CoverageBackend {
    /// Bumped on every area load; results of other generations are dropped.
    generation: u32,
    /// Jobs sent to the worker and not answered yet, by camera.
    in_flight: BTreeMap<u32, Job>,
    worker: Option<Worker>,
    /// Jobs submitted so far.
    #[cfg(any(test, feature = "testing"))]
    submitted: usize,
}

/// What happened to the worker during a [`CoverageBackend::drain`], for the
/// log, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerNote {
    /// It said it is listening; the requests held until now were posted.
    Ready,
    /// It can no longer be used, for this reason; coverage is computed
    /// inline from now on.
    Failed(String),
}

impl CoverageBackend {
    /// Sends jobs through `worker`, or computes them inline without one;
    /// `clock` times how long the worker takes to start.
    pub fn new(worker: Option<Box<dyn JobChannel>>, clock: Arc<dyn Clock>) -> Self {
        Self {
            worker: worker.map(|channel| Worker::new(channel, clock)),
            ..Self::default()
        }
    }

    /// Starts on a new area; results for earlier ones are dropped from now on.
    /// The worker gets the area's layer inputs at the given cell sizes.
    pub fn load_area(&mut self, bundle: &AreaBundle, ground_cell_m: f32, facade_cell_m: f32) {
        self.generation = self.generation.wrapping_add(1);
        self.in_flight.clear();
        if let Some(worker) = &mut self.worker {
            worker.send(&JobRequest::LoadArea {
                generation: self.generation,
                inputs: LayerInputs::new(bundle, ground_cell_m, facade_cell_m),
            });
        }
    }

    /// Whether a job for this camera has not come back yet.
    pub fn busy(&self, camera_id: u32) -> bool {
        self.in_flight.contains_key(&camera_id)
    }

    /// Jobs submitted so far.
    #[cfg(any(test, feature = "testing"))]
    pub fn submitted(&self) -> usize {
        self.submitted
    }

    /// Computes a camera's visibility: at once, returning it, when running
    /// inline; otherwise in the worker, to be collected by [`Self::drain`].
    pub fn submit(
        &mut self,
        camera_id: u32,
        key: u64,
        cone: ViewCone,
        subject_height_m: f32,
        layers: &StaticLayers,
    ) -> Option<JobResult> {
        let job = Job {
            generation: self.generation,
            camera_id,
            key,
            cone,
            subject_height_m,
        };
        #[cfg(any(test, feature = "testing"))]
        {
            self.submitted += 1;
        }
        if let Some(worker) = &mut self.worker {
            worker.send(&JobRequest::Compute(job));
            self.in_flight.insert(camera_id, job);
            return None;
        }
        Some(job.run(layers))
    }

    /// Results that came back since the last call. If the worker failed, the
    /// jobs it still owed are computed here instead, and it is not used again.
    pub fn drain(&mut self, layers: &StaticLayers) -> (Vec<JobResult>, Vec<WorkerNote>) {
        let mut out = Vec::new();
        let mut notes = Vec::new();
        if let Some(worker) = &mut self.worker {
            let failure = match worker.receive(&mut notes) {
                Ok(responses) => {
                    let mut failure = None;
                    for response in responses {
                        match response {
                            JobResponse::Done(r) => {
                                let current = r.generation == self.generation
                                    && self.in_flight.get(&r.camera_id).map(|j| j.key)
                                        == Some(r.key);
                                if current {
                                    self.in_flight.remove(&r.camera_id);
                                    out.push(r);
                                }
                            }
                            // Jobs of earlier areas left `in_flight` when the
                            // area changed. A job of this area means the worker
                            // missed it and would answer every resubmission the
                            // same way, so stop using it.
                            JobResponse::Stale { generation, .. }
                                if generation == self.generation =>
                            {
                                failure = Some("it does not hold the current area".to_owned());
                            }
                            _ => {}
                        }
                    }
                    failure
                }
                Err(e) => Some(e),
            };
            let Some(e) = failure else {
                return (out, notes);
            };
            notes.push(WorkerNote::Failed(e));
            self.worker = None;
        }
        // Running inline: only jobs a failed worker still owed can be left.
        out.extend(
            std::mem::take(&mut self.in_flight)
                .into_values()
                .map(|job| job.run(layers)),
        );
        (out, notes)
    }
}

/// The worker's side of the conversation: requests wait until it says it is
/// ready, and it must say so in time.
struct Worker {
    channel: Box<dyn JobChannel>,
    clock: Arc<dyn Clock>,
    /// Whether the worker has said it is listening.
    ready: bool,
    /// When the worker was started (Unix ms).
    started_ms: f64,
    /// Encoded requests waiting for the worker to be ready.
    queue: Vec<Vec<u8>>,
    /// A request that could not be posted.
    failed: Option<String>,
}

impl std::fmt::Debug for Worker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Worker")
            .field("ready", &self.ready)
            .field("started_ms", &self.started_ms)
            .field("queued", &self.queue.len())
            .field("failed", &self.failed)
            .finish_non_exhaustive()
    }
}

impl Worker {
    fn new(channel: Box<dyn JobChannel>, clock: Arc<dyn Clock>) -> Self {
        Self {
            channel,
            ready: false,
            started_ms: clock.unix_ms(),
            clock,
            queue: Vec::new(),
            failed: None,
        }
    }

    fn send(&mut self, request: &JobRequest) {
        let bytes = request.encode();
        if self.ready {
            self.post(&bytes);
        } else {
            self.queue.push(bytes);
        }
    }

    /// Posts a message; the first failure is reported by the next `receive`.
    fn post(&mut self, bytes: &[u8]) {
        if let Err(e) = self.channel.post(bytes) {
            self.failed.get_or_insert(e);
        }
    }

    /// Answers to jobs (`Done` and `Stale`) received since the last call,
    /// or why the worker can no longer be used.
    fn receive(&mut self, notes: &mut Vec<WorkerNote>) -> Result<Vec<JobResponse>, String> {
        let mut out = Vec::new();
        while let Some(reply) = self.channel.receive() {
            match JobResponse::decode(&reply?).map_err(|e| e.to_string())? {
                JobResponse::Ready => {
                    notes.push(WorkerNote::Ready);
                    self.ready = true;
                    for bytes in std::mem::take(&mut self.queue) {
                        self.post(&bytes);
                    }
                }
                JobResponse::Failed(e) => return Err(e),
                answer => out.push(answer),
            }
        }
        if !self.ready && self.clock.unix_ms() - self.started_ms > READY_WITHIN_MS {
            return Err(format!(
                "it did not start within {} s",
                READY_WITHIN_MS / 1000.0
            ));
        }
        self.failed.take().map_or(Ok(out), Err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{FakeClock, LoopbackJobChannel, ScriptedJobChannel};
    use observation_core::bundle::build_bundle;
    use observation_core::camera::{CameraKind, CameraModel, CameraPlacement};
    use observation_core::coverage_job::JOB_VERSION;
    use observation_core::geo::BBox;
    use observation_core::terrain::Terrain;

    const GROUND: f32 = 0.5;
    const FACADE: f32 = 1.0;

    fn bundle() -> AreaBundle {
        let bbox = BBox::new(51.5, -0.001, 51.5009, 0.001).expect("valid box");
        build_bundle(bbox, &[], &[], &[], Terrain::default(), "", 0, None)
    }

    fn layers() -> StaticLayers {
        StaticLayers::new(&bundle(), GROUND, FACADE)
    }

    /// A camera's cone; `x` tells cameras apart.
    fn cone(x: f32) -> ViewCone {
        let model = CameraModel {
            id: "test".into(),
            vendor: String::new(),
            name: "Test".into(),
            kind: CameraKind::Bullet,
            hfov_deg: 90.0,
            vfov_deg: 55.0,
            resolution: [1920, 1080],
            ir_range_m: None,
            source: None,
            notes: None,
        };
        let camera = CameraPlacement {
            id: 1,
            label: String::new(),
            model_id: "test".into(),
            position: [x, 3.0, 0.0],
            yaw_deg: 90.0,
            pitch_deg: -10.0,
            roll_deg: 0.0,
            color: None,
            enabled: true,
            group: None,
        };
        ViewCone::new(&camera, &model, 20.0)
    }

    fn job(generation: u32, camera_id: u32, key: u64) -> Job {
        Job {
            generation,
            camera_id,
            key,
            cone: cone(camera_id as f32),
            subject_height_m: 1.0,
        }
    }

    /// A backend over a scripted worker started at time 0, and the script.
    fn scripted() -> (CoverageBackend, ScriptedJobChannel, Arc<FakeClock>) {
        let channel = ScriptedJobChannel::default();
        let clock = Arc::new(FakeClock::at(0.0));
        let backend = CoverageBackend::new(Some(Box::new(channel.clone())), clock.clone());
        (backend, channel, clock)
    }

    /// Submits `job` (its generation is the backend's own).
    fn submit(b: &mut CoverageBackend, j: Job, layers: &StaticLayers) -> Option<JobResult> {
        b.submit(j.camera_id, j.key, j.cone, j.subject_height_m, layers)
    }

    fn done(j: Job, layers: &StaticLayers) -> JobResponse {
        JobResponse::Done(j.run(layers))
    }

    #[test]
    fn without_a_worker_jobs_run_at_once() {
        let layers = layers();
        let mut b = CoverageBackend::default();
        b.load_area(&bundle(), GROUND, FACADE);
        let j = job(1, 7, 3);
        assert_eq!(submit(&mut b, j, &layers), Some(j.run(&layers)));
        assert!(!b.busy(7));
        assert_eq!(b.drain(&layers), (vec![], vec![]));
        assert_eq!(b.submitted(), 1);
    }

    #[test]
    fn requests_wait_for_ready_and_then_go_in_order() {
        let layers = layers();
        let (mut b, channel, _) = scripted();
        b.load_area(&bundle(), GROUND, FACADE);
        let j = job(1, 7, 3);
        assert_eq!(submit(&mut b, j, &layers), None);
        assert!(b.busy(7));
        assert!(
            channel.posted().is_empty(),
            "held until the worker is ready"
        );

        channel.answer(&JobResponse::Ready);
        assert_eq!(b.drain(&layers), (vec![], vec![WorkerNote::Ready]));
        let inputs = LayerInputs::new(&bundle(), GROUND, FACADE);
        let area = JobRequest::LoadArea {
            generation: 1,
            inputs,
        };
        assert_eq!(channel.posted(), [area.clone(), JobRequest::Compute(j)]);

        // Once ready, a request goes at once.
        let k = job(1, 8, 4);
        assert_eq!(submit(&mut b, k, &layers), None);
        assert_eq!(channel.posted().len(), 3);
        channel.answer(&done(j, &layers));
        channel.answer(&done(k, &layers));
        assert_eq!(
            b.drain(&layers),
            (vec![j.run(&layers), k.run(&layers)], vec![])
        );
        assert!(!b.busy(7) && !b.busy(8));
    }

    #[test]
    fn the_area_goes_to_the_worker_only_when_there_is_one() {
        // Without a worker `load_area` builds no layer inputs; with one it
        // sends them, under the next generation each time.
        let (mut b, channel, _) = scripted();
        channel.answer(&JobResponse::Ready);
        b.drain(&layers());
        b.load_area(&bundle(), GROUND, FACADE);
        b.load_area(&bundle(), GROUND, FACADE);
        let generations: Vec<_> = channel
            .posted()
            .into_iter()
            .map(|r| match r {
                JobRequest::LoadArea { generation, inputs } => {
                    assert_eq!(inputs, LayerInputs::new(&bundle(), GROUND, FACADE));
                    generation
                }
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(generations, [1, 2]);
    }

    #[test]
    fn answers_for_another_area_or_an_older_pose_are_dropped() {
        let layers = layers();
        let (mut b, channel, _) = scripted();
        channel.answer(&JobResponse::Ready);
        b.load_area(&bundle(), GROUND, FACADE);
        submit(&mut b, job(1, 7, 3), &layers);
        // A new area: generation 2; the old job is no longer awaited.
        b.load_area(&bundle(), GROUND, FACADE);
        assert!(!b.busy(7));
        submit(&mut b, job(2, 7, 5), &layers);
        channel.answer(&done(job(1, 7, 3), &layers));
        channel.answer(&done(job(2, 7, 4), &layers));
        // An older area's Stale answer is not a failure.
        channel.answer(&JobResponse::Stale {
            generation: 1,
            camera_id: 7,
            key: 3,
        });
        assert_eq!(b.drain(&layers), (vec![], vec![WorkerNote::Ready]));
        assert!(b.busy(7), "still waiting for key 5");
        channel.answer(&done(job(2, 7, 5), &layers));
        assert_eq!(b.drain(&layers), (vec![job(2, 7, 5).run(&layers)], vec![]));
    }

    /// The worker is given up, and what it owed computed inline at once.
    fn gave_up(b: &mut CoverageBackend, layers: &StaticLayers, why: &str, owed: &[Job]) {
        let results: Vec<_> = owed.iter().map(|j| j.run(layers)).collect();
        let (out, notes) = b.drain(layers);
        assert_eq!(out, results);
        assert_eq!(notes.last(), Some(&WorkerNote::Failed(why.to_owned())));
        // From now on jobs run inline.
        let j = job(out.first().map_or(1, |r| r.generation), 99, 1);
        assert_eq!(submit(b, j, layers), Some(j.run(layers)));
        assert_eq!(b.drain(layers), (vec![], vec![]));
    }

    #[test]
    fn a_stale_answer_for_the_current_area_gives_the_worker_up() {
        let layers = layers();
        let (mut b, channel, _) = scripted();
        channel.answer(&JobResponse::Ready);
        b.load_area(&bundle(), GROUND, FACADE);
        let (j, k) = (job(1, 8, 3), job(1, 7, 4));
        submit(&mut b, j, &layers);
        submit(&mut b, k, &layers);
        // A result in the same pass is kept; the jobs still owed run inline,
        // in camera order.
        channel.answer(&done(j, &layers));
        channel.answer(&JobResponse::Stale {
            generation: 1,
            camera_id: 7,
            key: 4,
        });
        let (out, notes) = b.drain(&layers);
        assert_eq!(out, [j.run(&layers), k.run(&layers)]);
        assert_eq!(
            notes,
            [
                WorkerNote::Ready,
                WorkerNote::Failed("it does not hold the current area".into())
            ]
        );
        let i = job(1, 99, 1);
        assert_eq!(submit(&mut b, i, &layers), Some(i.run(&layers)));
    }

    #[test]
    fn an_error_drops_the_answers_of_its_pass_and_runs_the_owed_jobs_inline() {
        for (failure, why) in [
            (Err("worker panicked".to_owned()), "worker panicked"),
            (
                Ok(JobResponse::Failed("unreadable request".into()).encode()),
                "unreadable request",
            ),
            (
                Ok(vec![0xff, 1, 2]),
                &format!(
                    "coverage message version 255, but this build speaks version {JOB_VERSION}"
                ),
            ),
        ] {
            let layers = layers();
            let (mut b, channel, _) = scripted();
            channel.answer(&JobResponse::Ready);
            b.load_area(&bundle(), GROUND, FACADE);
            let (j, k) = (job(1, 7, 3), job(1, 8, 4));
            submit(&mut b, j, &layers);
            submit(&mut b, k, &layers);
            // Answered, but dropped with the rest of the pass: computed again.
            channel.answer(&done(j, &layers));
            channel.queue(failure);
            channel.answer(&done(k, &layers));
            gave_up(&mut b, &layers, why, &[j, k]);
            assert_eq!(b.submitted(), 3);
        }
    }

    #[test]
    fn a_worker_that_is_not_ready_within_ten_seconds_is_given_up() {
        let layers = layers();
        let (mut b, _channel, clock) = scripted();
        b.load_area(&bundle(), GROUND, FACADE);
        let j = job(1, 7, 3);
        submit(&mut b, j, &layers);
        clock.set(READY_WITHIN_MS);
        assert_eq!(
            b.drain(&layers),
            (vec![], vec![]),
            "exactly 10 s is in time"
        );
        clock.set(READY_WITHIN_MS + 1.0);
        gave_up(&mut b, &layers, "it did not start within 10 s", &[j]);

        // Once ready, the clock no longer matters.
        let (mut b, channel, clock) = scripted();
        channel.answer(&JobResponse::Ready);
        b.drain(&layers);
        clock.set(1e9);
        assert_eq!(b.drain(&layers), (vec![], vec![]));
    }

    #[test]
    fn a_refused_post_is_reported_by_the_next_drain() {
        let layers = layers();
        let (mut b, channel, _) = scripted();
        channel.answer(&JobResponse::Ready);
        b.drain(&layers);
        b.load_area(&bundle(), GROUND, FACADE);
        channel.refuse_posts(Some("detached buffer"));
        let j = job(1, 7, 3);
        assert_eq!(submit(&mut b, j, &layers), None, "not run here");
        assert!(b.busy(7));
        channel.refuse_posts(Some("a second failure"));
        submit(&mut b, job(1, 8, 4), &layers);
        // Answers of the same pass still count; only the first failure is told.
        channel.answer(&done(j, &layers));
        let (out, notes) = b.drain(&layers);
        assert_eq!(out, [j.run(&layers), job(1, 8, 4).run(&layers)]);
        assert_eq!(notes, [WorkerNote::Failed("detached buffer".into())]);
    }

    #[test]
    fn a_post_refused_while_flushing_the_queue_is_reported_in_the_same_pass() {
        let layers = layers();
        let (mut b, channel, _) = scripted();
        b.load_area(&bundle(), GROUND, FACADE);
        let j = job(1, 7, 3);
        submit(&mut b, j, &layers);
        channel.refuse_posts(Some("detached buffer"));
        channel.answer(&JobResponse::Ready);
        let (out, notes) = b.drain(&layers);
        assert_eq!(out, [j.run(&layers)]);
        assert_eq!(
            notes,
            [
                WorkerNote::Ready,
                WorkerNote::Failed("detached buffer".into())
            ]
        );
    }

    #[test]
    fn a_worker_answering_as_the_worker_binary_gives_the_inline_results() {
        let layers = layers();
        let mut inline = CoverageBackend::default();
        let mut worker = CoverageBackend::new(
            Some(Box::new(LoopbackJobChannel::default())),
            Arc::new(FakeClock::at(0.0)),
        );
        for b in [&mut inline, &mut worker] {
            b.load_area(&bundle(), GROUND, FACADE);
        }
        let jobs = [job(1, 7, 3), job(1, 8, 4), job(1, 9, 5)];
        let expected: Vec<_> = jobs
            .iter()
            .filter_map(|j| submit(&mut inline, *j, &layers))
            .collect();
        for j in jobs {
            assert_eq!(submit(&mut worker, j, &layers), None);
        }
        // The first drain sees Ready and posts the queue; the second the answers.
        assert_eq!(worker.drain(&layers), (vec![], vec![WorkerNote::Ready]));
        assert_eq!(worker.drain(&layers), (expected, vec![]));
        assert_eq!(worker.submitted(), inline.submitted());
    }
}
