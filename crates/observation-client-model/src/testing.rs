//! In-memory stand-ins for the [`crate::platform`] traits, for tests.
//!
//! Each fake records what the code under test asked for and lets the test
//! decide when and how it is answered.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::platform::{
    Clock, ColorScheme, FileDialogs, Http, HttpRequest, KeyValueStore, Reply, SaveFailure,
};

/// Locks a fake's state; a test that panicked while holding it has failed already.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

type Done = Box<dyn FnOnce(Reply) + Send>;
type Listener = Box<dyn Fn(Reply) + Send>;

/// Holds requests until the test answers them, in any order, or answers
/// each one at once (see [`FakeHttp::answering`]).
#[derive(Default)]
pub struct FakeHttp {
    /// Calls back at once with this, instead of holding the request.
    immediate: Option<Reply>,
    pending: Mutex<Vec<(HttpRequest, Done)>>,
    /// Every request ever sent, in order.
    sent: Mutex<Vec<HttpRequest>>,
    /// Subscriptions: url, event name, listener.
    streams: Mutex<Vec<(String, String, Listener)>>,
}

impl std::fmt::Debug for FakeHttp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeHttp")
            .field("sent", &*lock(&self.sent))
            .finish_non_exhaustive()
    }
}

impl FakeHttp {
    /// A fake that calls back with `reply` before `fetch` returns, as the
    /// native build does.
    pub fn answering(reply: Reply) -> Self {
        Self {
            immediate: Some(reply),
            ..Self::default()
        }
    }

    /// Every request sent so far, answered or not.
    pub fn sent(&self) -> Vec<HttpRequest> {
        lock(&self.sent).clone()
    }

    /// The URLs of the requests not answered yet, oldest first.
    pub fn pending_urls(&self) -> Vec<String> {
        lock(&self.pending)
            .iter()
            .map(|(r, _)| r.url.clone())
            .collect()
    }

    /// Answers the oldest pending request for `url`; false if there is none.
    pub fn respond(&self, url: &str, reply: Reply) -> bool {
        let done = {
            let mut pending = lock(&self.pending);
            let Some(i) = pending.iter().position(|(r, _)| r.url == url) else {
                return false;
            };
            pending.remove(i).1
        };
        // Called without the lock held, so the callback may send again.
        done(reply);
        true
    }

    /// Delivers `reply` to every subscriber of `url`; returns how many there
    /// were. The listeners run while the fake is locked, so they must not
    /// subscribe again.
    pub fn emit(&self, url: &str, reply: &Reply) -> usize {
        let streams = lock(&self.streams);
        let mut n = 0;
        for (_, _, on) in streams.iter().filter(|(u, _, _)| u == url) {
            on(reply.clone());
            n += 1;
        }
        n
    }

    /// The url and event name of every subscription, in order.
    pub fn subscriptions(&self) -> Vec<(String, String)> {
        lock(&self.streams)
            .iter()
            .map(|(u, e, _)| (u.clone(), e.clone()))
            .collect()
    }
}

impl Http for FakeHttp {
    fn fetch(&self, req: HttpRequest, done: Box<dyn FnOnce(Reply) + Send>) {
        lock(&self.sent).push(req.clone());
        match &self.immediate {
            Some(reply) => done(reply.clone()),
            None => lock(&self.pending).push((req, done)),
        }
    }

    fn subscribe(&self, url: &str, event: &str, on: Box<dyn Fn(Reply) + Send>) {
        lock(&self.streams).push((url.to_owned(), event.to_owned(), on));
    }
}

/// A key-value store in memory, which can be told to refuse writes.
#[derive(Debug, Default)]
pub struct MemoryStore {
    values: Mutex<HashMap<String, String>>,
    refuse: Mutex<Option<SaveFailure>>,
}

impl MemoryStore {
    /// From now on every `set` fails with `why` (or succeeds again with `None`).
    pub fn refuse_writes(&self, why: Option<SaveFailure>) {
        *lock(&self.refuse) = why;
    }

    /// A copy of everything stored.
    pub fn snapshot(&self) -> HashMap<String, String> {
        lock(&self.values).clone()
    }
}

impl KeyValueStore for MemoryStore {
    fn get(&self, key: &str) -> Option<String> {
        lock(&self.values).get(key).cloned()
    }

    fn set(&self, key: &str, value: &str) -> Result<(), SaveFailure> {
        if let Some(why) = *lock(&self.refuse) {
            return Err(why);
        }
        lock(&self.values).insert(key.to_owned(), value.to_owned());
        Ok(())
    }

    fn remove(&self, key: &str) {
        lock(&self.values).remove(key);
    }
}

type Picked = Box<dyn FnOnce(Result<String, String>) + Send>;

/// Records downloads and holds file pickers open until the test picks.
#[derive(Default)]
pub struct FakeFiles {
    /// Name, MIME type and text of every download.
    downloads: Mutex<Vec<(String, String, String)>>,
    /// Open pickers: accept list and callback.
    pickers: Mutex<Vec<(String, Picked)>>,
}

impl std::fmt::Debug for FakeFiles {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeFiles")
            .field("downloads", &*lock(&self.downloads))
            .finish_non_exhaustive()
    }
}

impl FakeFiles {
    /// Name, MIME type and text of every download so far.
    pub fn downloads(&self) -> Vec<(String, String, String)> {
        lock(&self.downloads).clone()
    }

    /// The accept lists of the pickers still open, oldest first.
    pub fn open_pickers(&self) -> Vec<String> {
        lock(&self.pickers).iter().map(|(a, _)| a.clone()).collect()
    }

    /// Completes the oldest open picker; false if none is open.
    pub fn pick(&self, result: Result<String, String>) -> bool {
        let done = {
            let mut pickers = lock(&self.pickers);
            if pickers.is_empty() {
                return false;
            }
            pickers.remove(0).1
        };
        done(result);
        true
    }
}

impl FileDialogs for FakeFiles {
    fn download(&self, name: &str, mime: &str, text: &str) -> Result<(), String> {
        lock(&self.downloads).push((name.to_owned(), mime.to_owned(), text.to_owned()));
        Ok(())
    }

    fn pick_text(&self, accept: &str, done: Box<dyn FnOnce(Result<String, String>) + Send>) {
        lock(&self.pickers).push((accept.to_owned(), done));
    }
}

/// A clock that only moves when the test says so.
#[derive(Debug, Default)]
pub struct FakeClock(Mutex<f64>);

impl FakeClock {
    pub fn at(unix_ms: f64) -> Self {
        Self(Mutex::new(unix_ms))
    }

    pub fn set(&self, unix_ms: f64) {
        *lock(&self.0) = unix_ms;
    }

    pub fn advance(&self, ms: f64) {
        *lock(&self.0) += ms;
    }
}

impl Clock for FakeClock {
    fn unix_ms(&self) -> f64 {
        *lock(&self.0)
    }
}

type SchemeListener = Box<dyn Fn(bool) + Send>;

/// A colour scheme the test flips.
#[derive(Default)]
pub struct FakeColorScheme {
    dark: Mutex<bool>,
    watchers: Mutex<Vec<SchemeListener>>,
}

impl std::fmt::Debug for FakeColorScheme {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeColorScheme")
            .field("dark", &*lock(&self.dark))
            .finish_non_exhaustive()
    }
}

impl FakeColorScheme {
    pub fn new(dark: bool) -> Self {
        Self {
            dark: Mutex::new(dark),
            watchers: Mutex::default(),
        }
    }

    /// Changes the setting and tells every watcher, as the browser does.
    pub fn set_dark(&self, dark: bool) {
        *lock(&self.dark) = dark;
        for on in lock(&self.watchers).iter() {
            on(dark);
        }
    }
}

impl ColorScheme for FakeColorScheme {
    fn prefers_dark(&self) -> bool {
        *lock(&self.dark)
    }

    fn watch(&self, on: Box<dyn Fn(bool) + Send>) {
        lock(&self.watchers).push(on);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::unix_secs;
    use std::sync::mpsc;

    #[test]
    fn http_answers_requests_in_order_through_the_callback() {
        let http = FakeHttp::default();
        let (tx, rx) = mpsc::channel();
        for url in ["/a", "/b", "/a"] {
            let tx = tx.clone();
            http.fetch(
                HttpRequest::get(url.to_owned()),
                Box::new(move |r| tx.send((url, r)).unwrap_or_default()),
            );
        }
        assert_eq!(http.pending_urls(), ["/a", "/b", "/a"]);
        assert!(http.respond("/a", Ok(b"one".to_vec())));
        assert!(http.respond("/b", Err("down".to_owned())));
        assert_eq!(http.pending_urls(), ["/a"]);
        assert!(!http.respond("/c", Ok(Vec::new())));
        let got: Vec<_> = rx.try_iter().collect();
        assert_eq!(
            got,
            [("/a", Ok(b"one".to_vec())), ("/b", Err("down".to_owned()))]
        );
        assert_eq!(http.sent().len(), 3);
    }

    #[test]
    fn http_can_answer_before_fetch_returns() {
        let http = FakeHttp::answering(Err("offline".to_owned()));
        let (tx, rx) = mpsc::channel();
        http.fetch(
            HttpRequest::get("/a".to_owned()),
            Box::new(move |r| tx.send(r).unwrap_or_default()),
        );
        assert_eq!(rx.try_recv(), Ok(Err("offline".to_owned())));
        assert!(http.pending_urls().is_empty());
        assert_eq!(http.sent().len(), 1);
    }

    #[test]
    fn requests_print_without_the_token() {
        let mut req = HttpRequest::get("/api/regions".to_owned());
        req.bearer = Some("secret-token".to_owned());
        let shown = format!("{req:?}");
        assert!(!shown.contains("secret-token"), "{shown}");
        assert!(shown.contains("<redacted>"), "{shown}");
    }

    #[test]
    fn http_streams_reach_every_subscriber_of_the_url() {
        let http = FakeHttp::default();
        let (tx, rx) = mpsc::channel();
        let tx2 = tx.clone();
        http.subscribe(
            "/events",
            "regions",
            Box::new(move |r| tx.send(r).unwrap_or_default()),
        );
        http.subscribe(
            "/other",
            "x",
            Box::new(move |r| tx2.send(r).unwrap_or_default()),
        );
        assert_eq!(http.emit("/events", &Ok(b"{}".to_vec())), 1);
        assert_eq!(http.emit("/events", &Err("dropped".to_owned())), 1);
        assert_eq!(
            rx.try_iter().collect::<Vec<_>>(),
            [Ok(b"{}".to_vec()), Err("dropped".to_owned())]
        );
        assert_eq!(
            http.subscriptions(),
            [
                ("/events".to_owned(), "regions".to_owned()),
                ("/other".to_owned(), "x".to_owned())
            ]
        );
    }

    #[test]
    fn store_keeps_removes_and_refuses() {
        let store = MemoryStore::default();
        assert_eq!(store.get("k"), None);
        assert_eq!(store.set("k", "v"), Ok(()));
        assert_eq!(store.get("k").as_deref(), Some("v"));
        store.refuse_writes(Some(SaveFailure::Full));
        assert_eq!(store.set("k", "w"), Err(SaveFailure::Full));
        assert_eq!(
            store.get("k").as_deref(),
            Some("v"),
            "a refused write keeps the old value"
        );
        store.refuse_writes(None);
        store.remove("k");
        assert_eq!(store.get("k"), None);
        store.remove("k");
        assert!(store.snapshot().is_empty());
    }

    #[test]
    fn files_record_downloads_and_answer_pickers_once() {
        let files = FakeFiles::default();
        assert_eq!(files.download("a.json", "application/json", "{}"), Ok(()));
        assert_eq!(
            files.downloads(),
            [(
                "a.json".to_owned(),
                "application/json".to_owned(),
                "{}".to_owned()
            )]
        );
        let (tx, rx) = mpsc::channel();
        files.pick_text(".json", Box::new(move |r| tx.send(r).unwrap_or_default()));
        assert_eq!(files.open_pickers(), [".json"]);
        assert!(files.pick(Ok("text".to_owned())));
        assert!(!files.pick(Ok("again".to_owned())));
        assert_eq!(rx.try_iter().collect::<Vec<_>>(), [Ok("text".to_owned())]);
    }

    #[test]
    fn clock_moves_only_when_told_and_rounds_down_to_seconds() {
        let clock = FakeClock::at(1_999.0);
        assert_eq!(unix_secs(&clock), 1);
        clock.advance(1.0);
        assert_eq!(unix_secs(&clock), 2);
        clock.set(0.0);
        assert_eq!(unix_secs(&clock), 0);
        clock.set(-5.0);
        assert_eq!(unix_secs(&clock), 0, "before the epoch counts as 0");
    }

    #[test]
    fn scheme_tells_watchers_of_a_change() {
        let scheme = FakeColorScheme::new(true);
        let (tx, rx) = mpsc::channel();
        scheme.watch(Box::new(move |d| tx.send(d).unwrap_or_default()));
        assert!(scheme.prefers_dark());
        scheme.set_dark(false);
        assert!(!scheme.prefers_dark());
        assert_eq!(rx.try_iter().collect::<Vec<_>>(), [false]);
    }
}
