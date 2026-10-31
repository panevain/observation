//! Where coverage jobs run: in a Web Worker in the browser, inline otherwise.
//!
//! [`CoverageState`](super::CoverageState) decides what to compute and keeps
//! the results; this only runs jobs. In the browser they go to the
//! `coverage-worker` binary (`src/bin/coverage_worker.rs`), so the frame never
//! waits for them. Natively, and whenever the worker cannot be started or
//! fails, they are computed inline on the main thread.

#[cfg(target_arch = "wasm32")]
use crate::state::{FACADE_CELL_M, GROUND_CELL_M};
use bevy::platform::collections::HashMap;
#[cfg(target_arch = "wasm32")]
use bevy::prelude::*;
use observation_core::bundle::AreaBundle;
use observation_core::coverage::StaticLayers;
use observation_core::coverage_job::{Job, JobResult};
#[cfg(target_arch = "wasm32")]
use observation_core::coverage_job::{JobRequest, JobResponse, LayerInputs};
use observation_core::frustum::ViewCone;

/// Runs coverage jobs. A non-send resource: in the browser it owns the `Worker`.
pub struct CoverageBackend {
    /// Bumped on every area load; results of other generations are dropped.
    generation: u32,
    /// Jobs sent to the worker and not answered yet, by camera.
    in_flight: HashMap<u32, Job>,
    #[cfg(target_arch = "wasm32")]
    worker: Option<web::WorkerLink>,
}

impl Default for CoverageBackend {
    fn default() -> Self {
        Self {
            generation: 0,
            in_flight: HashMap::default(),
            #[cfg(target_arch = "wasm32")]
            worker: web::WorkerLink::spawn(),
        }
    }
}

impl CoverageBackend {
    /// Starts on a new area; results for earlier ones are dropped from now on.
    #[cfg_attr(not(target_arch = "wasm32"), expect(unused_variables))]
    pub fn load_area(&mut self, bundle: &AreaBundle) {
        self.generation = self.generation.wrapping_add(1);
        self.in_flight.clear();
        #[cfg(target_arch = "wasm32")]
        if let Some(worker) = &mut self.worker {
            worker.send(&JobRequest::LoadArea {
                generation: self.generation,
                inputs: LayerInputs::new(bundle, GROUND_CELL_M, FACADE_CELL_M),
            });
        }
    }

    /// Whether a job for this camera has not come back yet.
    pub fn busy(&self, camera_id: u32) -> bool {
        self.in_flight.contains_key(&camera_id)
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
        #[cfg(target_arch = "wasm32")]
        if let Some(worker) = &mut self.worker {
            worker.send(&JobRequest::Compute(job));
            self.in_flight.insert(camera_id, job);
            return None;
        }
        Some(job.run(layers))
    }

    /// Results that came back since the last call. If the worker failed, the
    /// jobs it still owed are computed here instead, and it is not used again.
    pub fn drain(&mut self, layers: &StaticLayers) -> Vec<JobResult> {
        let mut out = Vec::new();
        #[cfg(target_arch = "wasm32")]
        if let Some(worker) = &mut self.worker {
            let failure = match worker.receive() {
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
            let Some(e) = failure else { return out };
            warn!("coverage worker failed ({e}); computing coverage on the main thread");
            self.worker = None;
        }
        // Running inline: only jobs a failed worker still owed can be left.
        out.extend(self.in_flight.drain().map(|(_, job)| job.run(layers)));
        out
    }
}

#[cfg(target_arch = "wasm32")]
mod web {
    use crate::net::js_cause;
    use bevy::prelude::*;
    use crossbeam_channel::{Receiver, unbounded};
    use observation_core::coverage_job::{JobRequest, JobResponse};
    use wasm_bindgen::JsCast;
    use wasm_bindgen::closure::Closure;
    use web_sys::{ErrorEvent, Event, MessageEvent, Worker, WorkerOptions, WorkerType};

    /// The loader trunk writes for the worker (`data-loader-shim` in
    /// `web/index.html`). Worker output is named after the binary, without a
    /// content hash, so the server has browsers revalidate these files.
    const LOADER_URL: &str = "/coverage-worker_loader.js";

    /// How long the worker may take to load and say it is ready, ms. A module
    /// worker whose script loads but whose wasm does not (an HTML page in its
    /// place, say) may never report an error.
    const READY_WITHIN_MS: f64 = 10_000.0;

    /// A message from the worker, or why it stopped.
    type Reply = Result<Vec<u8>, String>;

    pub struct WorkerLink {
        worker: Worker,
        rx: Receiver<Reply>,
        /// Whether the worker has said it is listening.
        ready: bool,
        /// When the worker was started (`Date.now()`, ms).
        started_ms: f64,
        /// Encoded requests waiting for the worker to be ready.
        queue: Vec<Vec<u8>>,
        /// A request that could not be posted.
        failed: Option<String>,
        // Called by the browser until dropped (after being detached, in `drop`).
        _on_message: Closure<dyn FnMut(MessageEvent)>,
        _on_error: Closure<dyn FnMut(Event)>,
    }

    impl WorkerLink {
        pub fn spawn() -> Option<Self> {
            let options = WorkerOptions::new();
            options.set_type(WorkerType::Module);
            options.set_name("coverage");
            let worker = match Worker::new_with_options(LOADER_URL, &options) {
                Ok(w) => w,
                Err(e) => {
                    warn!(
                        "could not start the coverage worker ({}); computing coverage on the main thread",
                        js_cause(&e)
                    );
                    return None;
                }
            };
            let (tx, rx) = unbounded();
            let on_message = {
                let tx = tx.clone();
                Closure::<dyn FnMut(MessageEvent)>::new(move |e: MessageEvent| {
                    // The receiver lives as long as the handler.
                    let _ = tx.send(Ok(js_sys::Uint8Array::new(&e.data()).to_vec()));
                })
            };
            // A script that fails to load or an uncaught error (a panic) in the worker.
            let on_error = Closure::<dyn FnMut(Event)>::new(move |e: Event| {
                let message = e
                    .dyn_ref::<ErrorEvent>()
                    .map(ErrorEvent::message)
                    .filter(|m| !m.is_empty())
                    .unwrap_or_else(|| format!("{LOADER_URL} could not be loaded"));
                let _ = tx.send(Err(message));
            });
            worker.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
            worker.set_onerror(Some(on_error.as_ref().unchecked_ref()));
            Some(Self {
                worker,
                rx,
                ready: false,
                started_ms: js_sys::Date::now(),
                queue: Vec::new(),
                failed: None,
                _on_message: on_message,
                _on_error: on_error,
            })
        }

        pub fn send(&mut self, request: &JobRequest) {
            let bytes = request.encode();
            if self.ready {
                self.post(&bytes);
            } else {
                self.queue.push(bytes);
            }
        }

        /// Posts a message, transferring (not copying) its buffer.
        fn post(&mut self, bytes: &[u8]) {
            let msg = js_sys::Uint8Array::from(bytes);
            let transfer = js_sys::Array::of1(&msg.buffer());
            if let Err(e) = self.worker.post_message_with_transfer(&msg, &transfer) {
                self.failed.get_or_insert_with(|| js_cause(&e));
            }
        }

        /// Answers to jobs (`Done` and `Stale`) received since the last call,
        /// or why the worker can no longer be used.
        pub fn receive(&mut self) -> Result<Vec<JobResponse>, String> {
            let mut out = Vec::new();
            while let Ok(reply) = self.rx.try_recv() {
                match JobResponse::decode(&reply?).map_err(|e| e.to_string())? {
                    JobResponse::Ready => {
                        info!("coverage worker ready");
                        self.ready = true;
                        for bytes in std::mem::take(&mut self.queue) {
                            self.post(&bytes);
                        }
                    }
                    JobResponse::Failed(e) => return Err(e),
                    answer => out.push(answer),
                }
            }
            if !self.ready && js_sys::Date::now() - self.started_ms > READY_WITHIN_MS {
                return Err(format!(
                    "it did not start within {} s",
                    READY_WITHIN_MS / 1000.0
                ));
            }
            self.failed.take().map_or(Ok(out), Err)
        }
    }

    impl Drop for WorkerLink {
        fn drop(&mut self) {
            // Detach the handlers before their closures are freed.
            self.worker.set_onmessage(None);
            self.worker.set_onerror(None);
            self.worker.terminate();
        }
    }
}
