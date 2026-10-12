//! A stand-in for a harness session's inbound socket, recording what is
//! posted to it.

use std::io::Read;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

/// A unix socket in `dir` that takes posts, one per connection, until
/// dropped.
pub struct FakeSocket {
    pub path: PathBuf,
    posts: mpsc::Receiver<String>,
}

impl FakeSocket {
    pub fn bind(dir: &Path) -> Self {
        let path = dir.join("s.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let (tx, posts) = mpsc::channel();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut post = String::new();
                if stream.read_to_string(&mut post).is_err() || tx.send(post).is_err() {
                    return;
                }
            }
        });
        Self { path, posts }
    }

    /// The next post, as the lines written to the socket.
    pub fn post(&self) -> String {
        self.posts.recv_timeout(Duration::from_secs(5)).unwrap()
    }

    /// Whether anything was posted within a moment.
    pub fn posted(&self) -> bool {
        self.posts.recv_timeout(Duration::from_millis(300)).is_ok()
    }
}
