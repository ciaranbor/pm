mod delivery;
mod subscription;

use super::*;

/// A device's push keys: the secret it decrypts with, and the
/// subscription JSON it registers.
fn subscriber(endpoint: &str) -> (web_push_native::p256::SecretKey, [u8; 16], String) {
    use base64ct::{Base64UrlUnpadded, Encoding};
    use web_push_native::p256::elliptic_curve::sec1::ToEncodedPoint;
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).unwrap();
    let secret = web_push_native::p256::SecretKey::from_slice(&bytes).unwrap();
    let mut auth = [0u8; 16];
    getrandom::fill(&mut auth).unwrap();
    let p256dh = secret.public_key().to_encoded_point(false);
    let json = serde_json::json!({
        "endpoint": endpoint,
        "keys": {
            "p256dh": Base64UrlUnpadded::encode_string(p256dh.as_bytes()),
            "auth": Base64UrlUnpadded::encode_string(&auth),
        },
    });
    (secret, auth, json.to_string())
}

fn stored_push(config: &Config, device: &str) -> Option<crate::state::devices::Push> {
    Devices::load(&config.files.devices()).unwrap().devices[device]
        .push
        .clone()
}

/// One request a push service received, and how it answered.
struct Received {
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Received {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// A push service on loopback answering each request with the next of
/// `statuses`; its URL, and the requests it received.
fn push_service(statuses: Vec<u16>) -> (String, mpsc::Receiver<Received>) {
    push_service_answering(
        statuses
            .into_iter()
            .map(|s| format!("HTTP/1.1 {s} X\r\nContent-Length: 0\r\n\r\n"))
            .collect(),
    )
}

/// [`push_service`], answering each request with the next raw response.
fn push_service_answering(responses: Vec<String>) -> (String, mpsc::Receiver<Received>) {
    use std::io::Read;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/push/abc", listener.local_addr().unwrap());
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for (stream, response) in listener.incoming().zip(responses) {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut headers = Vec::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let line = line.trim_end();
                if line.is_empty() {
                    break;
                }
                if let Some((name, value)) = line.split_once(": ") {
                    headers.push((name.to_string(), value.to_string()));
                }
            }
            let received = Received {
                headers,
                body: Vec::new(),
            };
            let length: usize = received
                .header("content-length")
                .map_or(0, |l| l.parse().unwrap());
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            stream.write_all(response.as_bytes()).unwrap();
            let _ = tx.send(Received { body, ..received });
        }
    });
    (url, rx)
}

/// Subscribe `device`, paired under `config`, to a push service at `url`.
fn subscribe(config: &Config, device: &str, url: &str) {
    let (_, _, subscription) = subscriber(url);
    let push = serve_push::subscription(&subscription, &PushPolicy::local()).unwrap();
    Devices::update(&config.files, |d| {
        d.devices.get_mut(device).unwrap().push = Some(push);
        Ok(())
    })
    .unwrap();
}
