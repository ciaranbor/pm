//! `GET /v1/events`: a server-sent event stream. Each stream starts with
//! the current snapshot, then gets a `snapshot` event whenever the snapshot
//! changes and a `transition` event for each [`Transition`] into it. A
//! stream that watches an agent also reads its conversation on a poll of
//! its own and gets a `transcript` event for what it gained
//! ([`TranscriptWatch`]). A comment line goes out whenever the stream has been
//! silent for the heartbeat interval, so proxies keep it open and a client
//! gone is found by the write that fails.

use std::io::{self, Write};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use crate::commands::attention::Snapshot;
use crate::commands::attention::transition::{Transition, Watch};
use crate::error::Result;

use super::transcript::TranscriptWatch;

pub(super) struct Hub {
    inner: Mutex<Inner>,
    /// How many streams are open.
    open: AtomicUsize,
}

struct Inner {
    /// The last snapshot, as JSON.
    snapshot: String,
    streams: Vec<Sender<String>>,
}

impl Hub {
    pub(super) fn new(snapshot: &Snapshot) -> Result<Self> {
        Ok(Self {
            inner: Mutex::new(Inner {
                snapshot: serde_json::to_string(snapshot)?,
                streams: Vec::new(),
            }),
            open: AtomicUsize::new(0),
        })
    }

    /// Whether a stream is open: someone is looking.
    pub(super) fn watched(&self) -> bool {
        self.open.load(Ordering::SeqCst) > 0
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A new stream's first event, and the receiver of the rest.
    fn subscribe(&self) -> (String, Receiver<String>) {
        let (tx, rx) = mpsc::channel();
        let mut inner = self.lock();
        inner.streams.push(tx);
        (event("snapshot", &inner.snapshot), rx)
    }

    /// Send every stream what changed from what `watch` last saw to
    /// `snapshot`, returning the watch after it and the transitions sent.
    pub(super) fn update(
        &self,
        watch: &Watch,
        snapshot: &Snapshot,
    ) -> Result<(Watch, Vec<Transition>)> {
        let json = serde_json::to_string(snapshot)?;
        let (next, transitions) = watch.observe(snapshot);
        let mut events = Vec::new();
        let mut inner = self.lock();
        if json != inner.snapshot {
            events.push(event("snapshot", &json));
            inner.snapshot = json;
        }
        for transition in &transitions {
            events.push(transition_event(transition)?);
        }
        for e in events {
            inner.streams.retain(|s| s.send(e.clone()).is_ok());
        }
        Ok((next, transitions))
    }
}

fn transition_event(transition: &Transition) -> Result<String> {
    Ok(event("transition", &serde_json::to_string(transition)?))
}

/// One event; `data` is compact JSON, so it fits one `data:` line.
fn event(name: &str, data: &str) -> String {
    format!("event: {name}\ndata: {data}\n\n")
}

/// Write an event stream to `out`, a raw response, until the client goes;
/// with a watch, reading its conversation every given interval.
pub(super) fn stream(
    out: &mut impl Write,
    hub: &Hub,
    heartbeat: Duration,
    mut watch: Option<(TranscriptWatch, Duration)>,
) -> io::Result<()> {
    let (first, events) = hub.subscribe();
    hub.open.fetch_add(1, Ordering::SeqCst);
    let _open = Open(&hub.open);
    let mut failing: Option<String> = None;
    // The watch's first read precedes the response, so a client that has
    // the opening snapshot knows the watch has begun.
    let opening = watch
        .as_mut()
        .and_then(|(watch, _)| transcript_event(watch, &mut failing));
    out.write_all(
        b"HTTP/1.1 200 OK\r\n\
          Content-Type: text/event-stream\r\n\
          Cache-Control: no-cache\r\n\
          Connection: close\r\n\r\n",
    )?;
    out.write_all(first.as_bytes())?;
    if let Some(opening) = opening {
        out.write_all(opening.as_bytes())?;
    }
    out.flush()?;
    let mut wrote = Instant::now();
    let mut next_poll = Instant::now() + watch.as_ref().map_or(Duration::ZERO, |(_, every)| *every);
    loop {
        if let Some((watch, every)) = watch.as_mut()
            && Instant::now() >= next_poll
        {
            next_poll = Instant::now() + *every;
            if let Some(e) = transcript_event(watch, &mut failing) {
                out.write_all(e.as_bytes())?;
                out.flush()?;
                wrote = Instant::now();
            }
        }
        let until_heartbeat = heartbeat.saturating_sub(wrote.elapsed());
        let wait = match &watch {
            Some(_) => until_heartbeat.min(next_poll.saturating_duration_since(Instant::now())),
            None => until_heartbeat,
        };
        match events.recv_timeout(wait) {
            Ok(e) => out.write_all(e.as_bytes())?,
            Err(RecvTimeoutError::Timeout) if wrote.elapsed() >= heartbeat => {
                out.write_all(b": heartbeat\n\n")?
            }
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }
        out.flush()?;
        wrote = Instant::now();
    }
}

/// A stream counted open until it ends.
struct Open<'a>(&'a AtomicUsize);

impl Drop for Open<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The `transcript` event for what `watch` finds changed, if anything. A
/// failure is logged once until it changes or the watch reads again.
fn transcript_event(watch: &mut TranscriptWatch, failing: &mut Option<String>) -> Option<String> {
    match watch.poll() {
        Ok(data) => {
            *failing = None;
            data.map(|data| event("transcript", &data))
        }
        Err(e) => {
            let e = e.to_string();
            if failing.as_ref() != Some(&e) {
                super::log(&format!("transcript unreadable: {e}"));
                *failing = Some(e);
            }
            None
        }
    }
}
