//! The browser build's side of [`crate::platform`]: `fetch`, `EventSource`,
//! `localStorage` and `sessionStorage`, file download and upload, the colour
//! scheme media query, the page's loading screen and the coverage worker.

use std::sync::Arc;

use super::{
    Clock, ColorScheme, FileDialogs, Http, HttpRequest, KeyValueStore, Platform, SaveFailure,
};
use bevy::log::warn;
use observation_client_model::platform::{JobChannel, Reply};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

pub fn platform() -> Platform {
    Platform {
        http: Arc::new(Fetch),
        local: Arc::new(LocalStorage),
        session: Arc::new(SessionStorage),
        files: Arc::new(BrowserFiles),
        clock: Arc::new(DateNow),
        scheme: Arc::new(MediaQuery),
    }
}

// Every port is a unit struct: each call looks up `window()` and what it
// needs afresh, and JS objects live only inside the closures handed to the
// browser. So the ports hold nothing that is tied to the main thread.

/// `fetch` and `EventSource`.
struct Fetch;

impl Http for Fetch {
    fn fetch(&self, req: HttpRequest, done: Box<dyn FnOnce(Reply) + Send>) {
        spawn_fetch(req, done);
    }

    fn subscribe(&self, url: &str, event: &str, on: Box<dyn Fn(Reply) + Send>) {
        spawn_event_source(url, event, on);
    }
}

/// `localStorage`.
struct LocalStorage;

impl KeyValueStore for LocalStorage {
    fn get(&self, key: &str) -> Option<String> {
        storage_get(key)
    }

    fn set(&self, key: &str, value: &str) -> Result<(), SaveFailure> {
        storage_set(key, value)
    }

    fn remove(&self, key: &str) {
        if let Some(s) = storage() {
            let _ = s.remove_item(key);
        }
    }
}

/// `sessionStorage`.
struct SessionStorage;

impl KeyValueStore for SessionStorage {
    fn get(&self, key: &str) -> Option<String> {
        session_get(key)
    }

    fn set(&self, key: &str, value: &str) -> Result<(), SaveFailure> {
        session_set(key, value)
    }

    fn remove(&self, key: &str) {
        session_remove(key);
    }
}

/// Downloads through a link to a `Blob`, and opens files with `<input type="file">`.
struct BrowserFiles;

impl FileDialogs for BrowserFiles {
    fn download(&self, name: &str, mime: &str, text: &str) -> Result<(), String> {
        download(name, mime, text)
    }

    fn pick_text(&self, accept: &str, done: Box<dyn FnOnce(Result<String, String>) + Send>) {
        pick_file(accept, done);
    }
}

/// `Date.now()`.
struct DateNow;

impl Clock for DateNow {
    fn unix_ms(&self) -> f64 {
        js_sys::Date::now()
    }
}

/// The `prefers-color-scheme` media query.
struct MediaQuery;

impl ColorScheme for MediaQuery {
    fn prefers_dark(&self) -> bool {
        prefers_dark()
    }

    fn watch(&self, on: Box<dyn Fn(bool) + Send>) {
        watch_scheme(on);
    }
}

// The loading screen.

/// Removes the HTML loading screen once the app is running.
pub fn hide_loader() {
    if let Some(el) = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.get_element_by_id("loader"))
    {
        el.remove();
    }
}

// Networking.

/// The message of a JavaScript exception (`TypeError: Failed to fetch`, a
/// `DOMException`, ...), or a placeholder when it carries none.
pub fn js_cause(e: &wasm_bindgen::JsValue) -> String {
    use wasm_bindgen::JsCast;
    let msg = match e.dyn_ref::<js_sys::Error>() {
        Some(err) => String::from(err.message()),
        None => e.as_string().unwrap_or_default(),
    };
    if msg.is_empty() {
        "no details from the browser".to_owned()
    } else {
        msg
    }
}

fn spawn_fetch(req: HttpRequest, done: impl FnOnce(Result<Vec<u8>, String>) + 'static) {
    use wasm_bindgen::JsCast;
    use wasm_bindgen_futures::JsFuture;

    wasm_bindgen_futures::spawn_local(async move {
        let result = async {
            let window = web_sys::window()
                .ok_or("the browser gave the app no window to make requests from")?;
            let init = web_sys::RequestInit::new();
            init.set_method(req.method);
            let headers = web_sys::Headers::new()
                .map_err(|e| format!("could not build the request: {}", js_cause(&e)))?;
            if let Some(body) = &req.body {
                let _ = headers.set("content-type", "application/json");
                init.set_body(&wasm_bindgen::JsValue::from_str(body));
            }
            if let Some(token) = req.bearer.as_deref().filter(|t| !t.is_empty()) {
                let _ = headers.set("authorization", &format!("Bearer {token}"));
            }
            init.set_headers(&headers);
            // A rejected fetch means no HTTP reply at all: offline, DNS, a dead
            // server, or a blocked connection.
            let resp: web_sys::Response =
                JsFuture::from(window.fetch_with_str_and_init(&req.url, &init))
                    .await
                    .map_err(|e| {
                        format!(
                            "the server did not answer (you may be offline, or it may be down): {}",
                            js_cause(&e)
                        )
                    })?
                    .dyn_into()
                    .map_err(|_| {
                        "the browser returned something other than a response".to_owned()
                    })?;
            if !resp.ok() {
                let body = match resp.text() {
                    Ok(text) => JsFuture::from(text)
                        .await
                        .ok()
                        .and_then(|v| v.as_string())
                        .unwrap_or_default(),
                    Err(_) => String::new(),
                };
                return Err(observation_client_model::net::http_failure(
                    resp.status(),
                    &body,
                ));
            }
            let read_err = |e: wasm_bindgen::JsValue| {
                format!(
                    "the connection dropped while the reply was downloading: {}",
                    js_cause(&e)
                )
            };
            let buf = JsFuture::from(resp.array_buffer().map_err(read_err)?)
                .await
                .map_err(read_err)?;
            Ok(js_sys::Uint8Array::new(&buf).to_vec())
        }
        .await
        .map_err(|e: String| e);
        done(result);
    });
}

/// Opens an `EventSource` and calls `on` with the data of every `event`
/// message, and with an error whenever the connection drops. The browser
/// retries a dropped stream by itself, but gives up for good after an error
/// answer (a 503 from the stream cap, a 502 from a proxy during a restart);
/// then a new one is opened after [`REOPEN_AFTER_MS`]. Lives as long as the page.
fn spawn_event_source(url: &str, event: &str, on: impl Fn(Result<Vec<u8>, String>) + 'static) {
    open_event_source(url.to_owned(), event.to_owned(), std::rc::Rc::new(on));
}

/// Wait before replacing an `EventSource` the browser closed.
const REOPEN_AFTER_MS: i32 = 5_000;

fn open_event_source(url: String, event: String, on: std::rc::Rc<dyn Fn(Result<Vec<u8>, String>)>) {
    use wasm_bindgen::JsCast;
    use wasm_bindgen::closure::Closure;

    let source = match web_sys::EventSource::new(&url) {
        Ok(s) => s,
        Err(e) => {
            on(Err(js_cause(&e)));
            return;
        }
    };
    let on_message = {
        let on = std::rc::Rc::clone(&on);
        Closure::<dyn Fn(web_sys::MessageEvent)>::new(move |e: web_sys::MessageEvent| {
            if let Some(data) = e.data().as_string() {
                on(Ok(data.into_bytes()));
            }
        })
    };
    let on_error = {
        let source = source.clone();
        let (url, event) = (url.clone(), event.clone());
        Closure::<dyn Fn(web_sys::Event)>::new(move |_: web_sys::Event| {
            on(Err("the live connection dropped".to_owned()));
            if source.ready_state() != web_sys::EventSource::CLOSED {
                return; // the browser is reconnecting
            }
            let (url, event, on) = (url.clone(), event.clone(), std::rc::Rc::clone(&on));
            let reopen = Closure::once_into_js(move || open_event_source(url, event, on));
            if let Some(window) = web_sys::window() {
                let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                    reopen.unchecked_ref(),
                    REOPEN_AFTER_MS,
                );
            }
        })
    };
    let _ = source.add_event_listener_with_callback(&event, on_message.as_ref().unchecked_ref());
    source.set_onerror(Some(on_error.as_ref().unchecked_ref()));
    // The handlers live as long as their source: for the page, or until it is
    // closed and replaced, which is rare enough not to matter.
    on_message.forget();
    on_error.forget();
}

// `localStorage` and files.

fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

fn storage_get(key: &str) -> Option<String> {
    storage()?.get_item(key).ok().flatten()
}

/// Whether the value was stored, and if not why. Quota and privacy errors
/// are non-fatal: the status bar says so and the user can still export.
fn storage_set(key: &str, value: &str) -> Result<(), SaveFailure> {
    let window = web_sys::window().ok_or(SaveFailure::Unavailable)?;
    // Reading `localStorage` itself throws when site data is blocked.
    let storage = match window.local_storage() {
        Ok(Some(s)) => s,
        Ok(None) => return Err(SaveFailure::Unavailable),
        Err(_) => return Err(SaveFailure::Blocked),
    };
    storage.set_item(key, value).map_err(|e| {
        let name = js_sys::Reflect::get(&e, &"name".into())
            .ok()
            .and_then(|n| n.as_string());
        match name.as_deref() {
            Some("QuotaExceededError" | "NS_ERROR_DOM_QUOTA_REACHED") => SaveFailure::Full,
            _ => SaveFailure::Blocked,
        }
    })
}

fn download(name: &str, mime: &str, text: &str) -> Result<(), String> {
    let doc = web_sys::window()
        .and_then(|w| w.document())
        .ok_or("the browser gave the app no page to download from")?;
    let parts = js_sys::Array::of1(&JsValue::from_str(text));
    let opts = web_sys::BlobPropertyBag::new();
    opts.set_type(mime);
    let blob = web_sys::Blob::new_with_str_sequence_and_options(&parts, &opts)
        .map_err(|e| format!("could not prepare the file ({})", js_cause(&e)))?;
    let url = web_sys::Url::create_object_url_with_blob(&blob)
        .map_err(|e| format!("could not prepare the download ({})", js_cause(&e)))?;
    let a = doc
        .create_element("a")
        .map(|e| e.unchecked_into::<web_sys::HtmlAnchorElement>())
        .map_err(|e| format!("could not start the download ({})", js_cause(&e)))?;
    a.set_href(&url);
    a.set_download(name);
    a.click();
    let _ = web_sys::Url::revoke_object_url(&url);
    Ok(())
}

fn pick_file(accept: &str, done: Box<dyn FnOnce(Result<String, String>) + Send>) {
    let Some(doc) = web_sys::window().and_then(|w| w.document()) else {
        done(Err(
            "the browser gave the app no page to ask from".to_owned()
        ));
        return;
    };
    let input = match doc
        .create_element("input")
        .map(|e| e.unchecked_into::<web_sys::HtmlInputElement>())
    {
        Ok(input) => input,
        Err(e) => {
            done(Err(format!(
                "could not open the file picker ({})",
                js_cause(&e)
            )));
            return;
        }
    };
    input.set_type("file");
    input.set_accept(accept);
    let el = input.clone();
    let on_change = Closure::once_into_js(move || {
        let Some(file) = el.files().and_then(|f| f.get(0)) else {
            return;
        };
        wasm_bindgen_futures::spawn_local(async move {
            let read = match wasm_bindgen_futures::JsFuture::from(file.text()).await {
                Ok(v) => v.as_string().ok_or_else(|| "it is not text".to_owned()),
                Err(e) => Err(js_cause(&e)),
            };
            done(read);
        });
    });
    input.set_onchange(Some(on_change.unchecked_ref()));
    input.click();
}

// `sessionStorage`.

fn session_storage() -> Option<web_sys::Storage> {
    web_sys::window()?.session_storage().ok().flatten()
}

fn session_get(key: &str) -> Option<String> {
    session_storage()?.get_item(key).ok().flatten()
}

fn session_set(key: &str, value: &str) -> Result<(), SaveFailure> {
    let s = session_storage().ok_or(SaveFailure::Unavailable)?;
    s.set_item(key, value).map_err(|_| SaveFailure::Blocked)
}

fn session_remove(key: &str) {
    if let Some(s) = session_storage() {
        let _ = s.remove_item(key);
    }
}

// The colour scheme.

fn dark_query() -> Option<web_sys::MediaQueryList> {
    web_sys::window().and_then(|w| w.match_media("(prefers-color-scheme: dark)").ok().flatten())
}

fn prefers_dark() -> bool {
    dark_query().is_some_and(|m| m.matches())
}

/// Calls `on` with the new setting whenever the media query's `change` event fires.
fn watch_scheme(on: Box<dyn Fn(bool) + Send>) {
    use wasm_bindgen::JsCast;
    use wasm_bindgen::closure::Closure;

    let Some(query) = dark_query() else { return };
    let on_change = {
        let query = query.clone();
        Closure::<dyn Fn(web_sys::Event)>::new(move |_: web_sys::Event| {
            on(query.matches());
        })
    };
    if let Err(e) =
        query.add_event_listener_with_callback("change", on_change.as_ref().unchecked_ref())
    {
        warn!("cannot follow the system theme: {}", js_cause(&e));
        return;
    }
    // The handler (and the query it holds, which must stay alive to keep
    // firing) lives as long as the page.
    on_change.forget();
}

// The coverage worker.

/// The loader trunk writes for the worker (`data-loader-shim` in
/// `web/index.html`). Worker output is named after the binary, without a
/// content hash, so the server has browsers revalidate these files.
const LOADER_URL: &str = "/coverage-worker_loader.js";

/// Starts the `coverage-worker` binary in a Web Worker, or logs why it could
/// not (coverage is then computed on the main thread).
pub fn coverage_worker() -> Option<Box<dyn JobChannel>> {
    WorkerLink::spawn().map(|link| Box::new(link) as Box<dyn JobChannel>)
}

/// The Web Worker and the handlers that pass its messages on. Owned by the
/// coverage backend alone, which drops it when the worker fails.
struct WorkerLink {
    worker: web_sys::Worker,
    rx: crossbeam_channel::Receiver<Reply>,
    // Called by the browser until dropped (after being detached, in `drop`).
    _on_message: Closure<dyn FnMut(web_sys::MessageEvent)>,
    _on_error: Closure<dyn FnMut(web_sys::Event)>,
}

impl WorkerLink {
    fn spawn() -> Option<Self> {
        use web_sys::{ErrorEvent, Event, MessageEvent, Worker, WorkerOptions, WorkerType};
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
        let (tx, rx) = crossbeam_channel::unbounded();
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
            _on_message: on_message,
            _on_error: on_error,
        })
    }
}

impl JobChannel for WorkerLink {
    /// Posts a message, transferring (not copying) its buffer.
    fn post(&mut self, bytes: &[u8]) -> Result<(), String> {
        let msg = js_sys::Uint8Array::from(bytes);
        let transfer = js_sys::Array::of1(&msg.buffer());
        self.worker
            .post_message_with_transfer(&msg, &transfer)
            .map_err(|e| js_cause(&e))
    }

    fn receive(&mut self) -> Option<Reply> {
        self.rx.try_recv().ok()
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
