//! Web Worker that computes per-camera coverage off the main thread.
//!
//! Speaks the [`observation_core::coverage_job`] protocol: one postcard
//! message per `postMessage`, as a `Uint8Array` whose buffer is transferred.
//! Built by trunk from `web/index.html` (`data-type="worker"`) and started by
//! the app's coverage backend; uses no Bevy.

#[cfg(target_arch = "wasm32")]
fn main() {
    worker::run();
}

/// The worker only exists in the browser build.
#[cfg(not(target_arch = "wasm32"))]
fn main() {}

#[cfg(target_arch = "wasm32")]
mod worker {
    use observation_core::bundle::AreaBundle;
    use observation_core::coverage::StaticLayers;
    use observation_core::coverage_job::{JobRequest, JobResponse};
    use wasm_bindgen::JsCast;
    use wasm_bindgen::closure::Closure;
    use web_sys::{DedicatedWorkerGlobalScope, MessageEvent};

    pub fn run() {
        let scope: DedicatedWorkerGlobalScope = js_sys::global().unchecked_into();
        let reply = {
            let scope = scope.clone();
            move |resp: JobResponse| {
                let bytes = js_sys::Uint8Array::from(resp.encode().as_slice());
                // Only a detached buffer fails to post, and this one is new.
                let _ =
                    scope.post_message_with_transfer(&bytes, &js_sys::Array::of1(&bytes.buffer()));
            }
        };
        // The analysis layers of the current area, with its generation.
        let mut area: Option<(u32, StaticLayers)> = None;
        let on_message = Closure::<dyn FnMut(MessageEvent)>::new(move |e: MessageEvent| {
            let bytes = js_sys::Uint8Array::new(&e.data()).to_vec();
            match JobRequest::decode(&bytes) {
                Ok(JobRequest::LoadArea {
                    generation,
                    bundle,
                    ground_cell_m,
                    facade_cell_m,
                }) => match AreaBundle::decode(&bundle) {
                    Ok(bundle) => {
                        let layers = StaticLayers::new(&bundle, ground_cell_m, facade_cell_m);
                        area = Some((generation, layers));
                    }
                    Err(e) => reply(JobResponse::Failed(e.to_string())),
                },
                Ok(JobRequest::Compute(job)) => {
                    // Jobs for an area that has since been replaced are dropped.
                    if let Some((_, layers)) = area.as_ref().filter(|(g, _)| *g == job.generation) {
                        reply(JobResponse::Done(job.run(layers)));
                    }
                }
                Err(e) => reply(JobResponse::Failed(e.to_string())),
            }
        });
        scope.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
        // The handler lives as long as the worker.
        on_message.forget();
        // Messages that arrived while the module was loading found no handler
        // and were dropped, so the app sends nothing until it sees this.
        let ready = js_sys::Uint8Array::from(JobResponse::Ready.encode().as_slice());
        let _ = scope.post_message(&ready);
    }
}
