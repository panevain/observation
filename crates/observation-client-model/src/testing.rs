//! In-memory stand-ins for the [`crate::platform`] traits, for tests.
//!
//! Each fake records what the code under test asked for and lets the test
//! decide when and how it is answered.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::platform::{
    Clock, ColorScheme, FileDialogs, Http, HttpRequest, JobChannel, KeyValueStore, Reply,
    SaveFailure,
};
use observation_core::coverage::StaticLayers;
use observation_core::coverage_job::{JobRequest, JobResponse};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

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
            .field("pending", &self.pending())
            .finish_non_exhaustive()
    }
}

/// Runs `call` on each listener taken out of `slot`, without holding its lock,
/// so a listener may register another; then puts them back ahead of any
/// registered meanwhile.
fn call_unlocked<T>(slot: &Mutex<Vec<T>>, mut call: impl FnMut(&T)) {
    let taken = std::mem::take(&mut *lock(slot));
    for item in &taken {
        call(item);
    }
    let mut now = lock(slot);
    let added = std::mem::replace(&mut *now, taken);
    now.extend(added);
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

    /// The requests not answered yet, oldest first.
    pub fn pending(&self) -> Vec<HttpRequest> {
        lock(&self.pending).iter().map(|(r, _)| r.clone()).collect()
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
        self.respond_where(|r| r.url == url, reply)
    }

    /// Answers the oldest pending `method` request for `url`; false if there is none.
    pub fn respond_to(&self, method: &str, url: &str, reply: Reply) -> bool {
        self.respond_where(|r| r.method == method && r.url == url, reply)
    }

    /// Answers the pending request at `index` (oldest first, as in
    /// [`Self::pending`]); false if there is none.
    pub fn respond_at(&self, index: usize, reply: Reply) -> bool {
        let done = {
            let mut pending = lock(&self.pending);
            if index >= pending.len() {
                return false;
            }
            pending.remove(index).1
        };
        // Called without the lock held, so the callback may send again.
        done(reply);
        true
    }

    fn respond_where(&self, matches: impl Fn(&HttpRequest) -> bool, reply: Reply) -> bool {
        let index = lock(&self.pending).iter().position(|(r, _)| matches(r));
        index.is_some_and(|i| self.respond_at(i, reply))
    }

    /// Delivers `reply` to every subscriber of `url`; returns how many there
    /// were. A subscriber may subscribe again or fetch from inside.
    pub fn emit(&self, url: &str, reply: &Reply) -> usize {
        let mut n = 0;
        call_unlocked(&self.streams, |(u, _, on)| {
            if u == url {
                on(reply.clone());
                n += 1;
            }
        });
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

/// A key-value store in memory, which can be told to refuse writes or to
/// read nothing (as blocked browser storage does).
#[derive(Debug, Default)]
pub struct MemoryStore {
    values: Mutex<HashMap<String, String>>,
    refuse: Mutex<Option<SaveFailure>>,
    blocked_reads: Mutex<bool>,
    log: Mutex<Vec<StoreOp>>,
}

/// One write to a [`MemoryStore`], as [`MemoryStore::ops`] lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreOp {
    /// A stored key and value.
    Set(String, String),
    /// A write refused (see [`MemoryStore::refuse_writes`]): key and value.
    Refused(String, String),
    /// A removed key (whether or not it was there).
    Remove(String),
}

impl MemoryStore {
    /// Every write since the store was made or last drained, oldest first.
    pub fn ops(&self) -> Vec<StoreOp> {
        lock(&self.log).clone()
    }

    /// Like [`Self::ops`], and starts a new log.
    pub fn take_ops(&self) -> Vec<StoreOp> {
        std::mem::take(&mut *lock(&self.log))
    }

    /// From now on every `get` finds nothing (or finds what is stored again
    /// with `false`).
    pub fn block_reads(&self, blocked: bool) {
        *lock(&self.blocked_reads) = blocked;
    }

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
        if *lock(&self.blocked_reads) {
            return None;
        }
        lock(&self.values).get(key).cloned()
    }

    fn set(&self, key: &str, value: &str) -> Result<(), SaveFailure> {
        let refused = *lock(&self.refuse);
        let (key, value) = (key.to_owned(), value.to_owned());
        if let Some(why) = refused {
            lock(&self.log).push(StoreOp::Refused(key, value));
            return Err(why);
        }
        lock(&self.log).push(StoreOp::Set(key.clone(), value.clone()));
        lock(&self.values).insert(key, value);
        Ok(())
    }

    fn remove(&self, key: &str) {
        lock(&self.log).push(StoreOp::Remove(key.to_owned()));
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
    /// Why every download fails, when set.
    refuse: Mutex<Option<String>>,
}

impl std::fmt::Debug for FakeFiles {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeFiles")
            .field("downloads", &*lock(&self.downloads))
            .finish_non_exhaustive()
    }
}

impl FakeFiles {
    /// From now on every download fails with `why` (or succeeds again with
    /// `None`); a refused download is not recorded.
    pub fn refuse_downloads(&self, why: Option<&str>) {
        *lock(&self.refuse) = why.map(str::to_owned);
    }

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
        if let Some(why) = lock(&self.refuse).clone() {
            return Err(why);
        }
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
        call_unlocked(&self.watchers, |on| on(dark));
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

/// A [`Tracked`](crate::commands::Tracked) value that counts write
/// access, which the client's resources record as a change.
#[derive(Debug, Default)]
pub struct Counting<T> {
    pub value: T,
    pub writes: usize,
}

impl<T> Counting<T> {
    pub fn new(value: T) -> Self {
        Self { value, writes: 0 }
    }
}

impl<T> crate::commands::Tracked<T> for Counting<T> {
    fn get(&self) -> &T {
        &self.value
    }

    fn get_mut(&mut self) -> &mut T {
        self.writes += 1;
        &mut self.value
    }
}

/// A coverage worker the test plays: it records what is posted and hands
/// back the messages the test queues. Clones share the script, so the test
/// keeps one while the backend owns another.
#[derive(Debug, Clone, Default)]
pub struct ScriptedJobChannel(Rc<RefCell<Script>>);

#[derive(Debug, Default)]
struct Script {
    posted: Vec<Vec<u8>>,
    inbox: VecDeque<Reply>,
    refuse: Option<String>,
}

impl ScriptedJobChannel {
    /// Every request posted so far, decoded, oldest first.
    pub fn posted(&self) -> Vec<JobRequest> {
        let script = self.0.borrow();
        let decode = |b: &Vec<u8>| JobRequest::decode(b).expect("the client posts requests");
        script.posted.iter().map(decode).collect()
    }

    /// Queues an answer from the worker.
    pub fn answer(&self, response: &JobResponse) {
        self.queue(Ok(response.encode()));
    }

    /// Queues a raw message, or an error event (`Err`).
    pub fn queue(&self, reply: Reply) {
        self.0.borrow_mut().inbox.push_back(reply);
    }

    /// From now on every post fails with `why` (or succeeds again with `None`);
    /// a refused post is not recorded.
    pub fn refuse_posts(&self, why: Option<&str>) {
        self.0.borrow_mut().refuse = why.map(str::to_owned);
    }
}

impl JobChannel for ScriptedJobChannel {
    fn post(&mut self, bytes: &[u8]) -> Result<(), String> {
        let mut script = self.0.borrow_mut();
        if let Some(why) = &script.refuse {
            return Err(why.clone());
        }
        script.posted.push(bytes.to_vec());
        Ok(())
    }

    fn receive(&mut self) -> Option<Reply> {
        self.0.borrow_mut().inbox.pop_front()
    }
}

/// A coverage worker that answers as `coverage-worker` does, in-process:
/// `Ready` first, then each request in turn (an area's layers are built from
/// its inputs; a job of another generation is `Stale`). As from a browser
/// worker, answers arrive in a later receiving pass than the one their
/// request was posted in (a pass ends when `receive` says `None`).
pub struct LoopbackJobChannel {
    area: Option<(u32, StaticLayers)>,
    inbox: VecDeque<Reply>,
    /// Answers not delivered yet: they arrive when the next pass starts.
    next: VecDeque<Reply>,
    /// A receiving pass has started and not ended.
    in_pass: bool,
}

impl std::fmt::Debug for LoopbackJobChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoopbackJobChannel")
            .field("generation", &self.area.as_ref().map(|(g, _)| g))
            .field("inbox", &self.inbox.len())
            .field("next", &self.next.len())
            .finish()
    }
}

impl Default for LoopbackJobChannel {
    fn default() -> Self {
        Self {
            area: None,
            inbox: VecDeque::from([Ok(JobResponse::Ready.encode())]),
            next: VecDeque::new(),
            in_pass: false,
        }
    }
}

impl JobChannel for LoopbackJobChannel {
    fn post(&mut self, bytes: &[u8]) -> Result<(), String> {
        let response = match JobRequest::decode(bytes) {
            Ok(JobRequest::LoadArea { generation, inputs }) => {
                self.area = Some((generation, inputs.build()));
                return Ok(());
            }
            Ok(JobRequest::Compute(job)) => match &self.area {
                Some((g, layers)) if *g == job.generation => JobResponse::Done(job.run(layers)),
                _ => JobResponse::Stale {
                    generation: job.generation,
                    camera_id: job.camera_id,
                    key: job.key,
                },
            },
            Err(e) => JobResponse::Failed(e.to_string()),
        };
        self.next.push_back(Ok(response.encode()));
        Ok(())
    }

    fn receive(&mut self) -> Option<Reply> {
        if !self.in_pass {
            self.inbox.append(&mut self.next);
            self.in_pass = true;
        }
        let reply = self.inbox.pop_front();
        self.in_pass = reply.is_some();
        reply
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::unix_secs;
    use std::sync::{Arc, mpsc};

    /// A reply handler that asks for the next request, as the tile queue
    /// does: each reply to `/n` fetches `/n+1`, up to `/3`.
    fn fetch_chain(http: &Arc<FakeHttp>, n: u32, log: mpsc::Sender<(u32, Reply)>) {
        let next = Arc::clone(http);
        http.fetch(
            HttpRequest::get(format!("/{n}")),
            Box::new(move |r| {
                log.send((n, r)).unwrap_or_default();
                if n < 3 {
                    fetch_chain(&next, n + 1, log);
                }
            }),
        );
    }

    #[test]
    fn a_deferred_reply_handler_may_fetch_again() {
        let http = Arc::new(FakeHttp::default());
        let (tx, rx) = mpsc::channel();
        fetch_chain(&http, 1, tx);
        for n in 1..=3 {
            assert_eq!(http.pending_urls(), [format!("/{n}")]);
            assert!(http.respond(&format!("/{n}"), Ok(vec![n as u8])));
        }
        assert!(http.pending_urls().is_empty());
        let got: Vec<u32> = rx.try_iter().map(|(n, _)| n).collect();
        assert_eq!(got, [1, 2, 3]);
    }

    #[test]
    fn an_immediate_reply_handler_may_fetch_again() {
        let http = Arc::new(FakeHttp::answering(Err("offline".to_owned())));
        let (tx, rx) = mpsc::channel();
        fetch_chain(&http, 1, tx);
        let got: Vec<(u32, Reply)> = rx.try_iter().collect();
        assert_eq!(
            got,
            (1..=3)
                .map(|n| (n, Err("offline".to_owned())))
                .collect::<Vec<_>>()
        );
        assert_eq!(http.sent().len(), 3);
        assert!(http.pending().is_empty());
    }

    #[test]
    fn requests_are_answered_by_method_and_url_or_position() {
        let http = FakeHttp::default();
        let (tx, rx) = mpsc::channel();
        let mut post = HttpRequest::get("/r".to_owned());
        post.method = "POST";
        for req in [
            HttpRequest::get("/r".to_owned()),
            post,
            HttpRequest::get("/s".to_owned()),
        ] {
            let tx = tx.clone();
            let label = format!("{} {}", req.method, req.url);
            http.fetch(req, Box::new(move |_| tx.send(label).unwrap_or_default()));
        }
        assert!(!http.respond_to("PUT", "/r", Ok(Vec::new())));
        assert!(http.respond_to("POST", "/r", Ok(Vec::new())));
        assert!(http.respond_at(1, Ok(Vec::new())));
        assert!(!http.respond_at(1, Ok(Vec::new())));
        assert_eq!(http.pending_urls(), ["/r"]);
        assert_eq!(http.pending()[0].method, "GET");
        assert_eq!(rx.try_iter().collect::<Vec<_>>(), ["POST /r", "GET /s"]);
    }

    #[test]
    fn a_stream_listener_may_subscribe_again() {
        let http = Arc::new(FakeHttp::default());
        let again = Arc::clone(&http);
        http.subscribe(
            "/events",
            "e",
            Box::new(move |_| again.subscribe("/events", "e", Box::new(|_| {}))),
        );
        assert_eq!(http.emit("/events", &Ok(Vec::new())), 1);
        assert_eq!(http.subscriptions().len(), 2);
        assert_eq!(http.emit("/events", &Ok(Vec::new())), 2);
        assert_eq!(http.subscriptions().len(), 3);
    }

    #[test]
    fn a_scheme_watcher_may_watch_again() {
        let scheme = Arc::new(FakeColorScheme::new(true));
        let again = Arc::clone(&scheme);
        let (tx, rx) = mpsc::channel();
        scheme.watch(Box::new(move |d| {
            let tx = tx.clone();
            again.watch(Box::new(move |d2| tx.send(d2).unwrap_or_default()));
            let _ = d;
        }));
        scheme.set_dark(false);
        scheme.set_dark(true);
        assert_eq!(rx.try_iter().collect::<Vec<_>>(), [true]);
    }

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
        let http = FakeHttp::default();
        http.fetch(req.clone(), Box::new(|_| {}));
        for shown in [
            format!("{req:?}"),
            format!("{req:#?}"),
            format!("{:?}", vec![req.clone()]),
            format!("{http:?}"),
            format!("{http:#?}"),
        ] {
            assert!(!shown.contains("secret-token"), "{shown}");
            assert!(shown.contains("<redacted>"), "{shown}");
        }
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
        assert_eq!(store.set("k", "v"), Ok(()));
        store.block_reads(true);
        assert_eq!(store.get("k"), None, "blocked storage reads nothing");
        store.block_reads(false);
        assert_eq!(store.get("k").as_deref(), Some("v"));
        let set = |k: &str, v: &str| StoreOp::Set(k.to_owned(), v.to_owned());
        assert_eq!(
            store.take_ops(),
            [
                set("k", "v"),
                StoreOp::Refused("k".to_owned(), "w".to_owned()),
                StoreOp::Remove("k".to_owned()),
                StoreOp::Remove("k".to_owned()),
                set("k", "v"),
            ]
        );
        assert_eq!(store.ops(), [], "taking the log starts a new one");
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
        files.refuse_downloads(Some("disk full"));
        assert_eq!(
            files.download("b.json", "application/json", "{}"),
            Err("disk full".to_owned())
        );
        files.refuse_downloads(None);
        assert_eq!(
            files.downloads().len(),
            1,
            "a refused download is not recorded"
        );
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
