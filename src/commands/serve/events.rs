//! `GET /v1/events`: a server-sent event stream. Each stream starts with
//! the current snapshot, then gets a `snapshot` event whenever the snapshot
//! changes and a `transition` event for each [`Transition`] into it. A
//! comment line goes out whenever the stream has been silent for the
//! heartbeat interval, so proxies keep it open and a client gone is found
//! by the write that fails.

use std::io::{self, Write};
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

use crate::commands::attention::Snapshot;
use crate::commands::attention::transition::{Transition, Watch};
use crate::error::Result;

pub(super) struct Hub {
    inner: Mutex<Inner>,
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
        })
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
    /// `snapshot`, returning the watch after it.
    pub(super) fn update(&self, watch: &Watch, snapshot: &Snapshot) -> Result<Watch> {
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
        Ok(next)
    }
}

fn transition_event(transition: &Transition) -> Result<String> {
    Ok(event("transition", &serde_json::to_string(transition)?))
}

/// One event; `data` is compact JSON, so it fits one `data:` line.
fn event(name: &str, data: &str) -> String {
    format!("event: {name}\ndata: {data}\n\n")
}

/// Write an event stream to `out`, a raw response, until the client goes.
pub(super) fn stream(out: &mut impl Write, hub: &Hub, heartbeat: Duration) -> io::Result<()> {
    let (first, events) = hub.subscribe();
    out.write_all(
        b"HTTP/1.1 200 OK\r\n\
          Content-Type: text/event-stream\r\n\
          Cache-Control: no-cache\r\n\
          Connection: close\r\n\r\n",
    )?;
    out.write_all(first.as_bytes())?;
    out.flush()?;
    loop {
        match events.recv_timeout(heartbeat) {
            Ok(e) => out.write_all(e.as_bytes())?,
            Err(RecvTimeoutError::Timeout) => out.write_all(b": heartbeat\n\n")?,
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }
        out.flush()?;
    }
}
