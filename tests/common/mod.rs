//! A fake adb server for the `scripts/phone` tests.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

/// An adb server on a free port answering `host:devices`, for as long as
/// the test runs.
pub struct AdbServer {
    pub port: u16,
    up: Arc<AtomicBool>,
}

impl AdbServer {
    pub fn start(devices: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let up = Arc::new(AtomicBool::new(true));
        let serving = Arc::clone(&up);
        thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                if serving.load(Ordering::SeqCst) {
                    let _ = answer(conn, devices);
                }
            }
        });
        Self { port, up }
    }

    /// While down, a connection is closed unanswered.
    #[allow(dead_code)]
    pub fn set_up(&self, up: bool) {
        self.up.store(up, Ordering::SeqCst);
    }
}

fn answer(mut conn: TcpStream, devices: &str) -> std::io::Result<()> {
    let mut len = [0u8; 4];
    conn.read_exact(&mut len)?;
    let len = usize::from_str_radix(std::str::from_utf8(&len).unwrap(), 16).unwrap();
    let mut req = vec![0u8; len];
    conn.read_exact(&mut req)?;
    assert_eq!(req, b"host:devices");
    write!(conn, "OKAY{:04x}{devices}", devices.len())
}

/// A port nothing listens on.
pub fn closed_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}
