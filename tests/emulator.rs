//! `scripts/emulator`'s isolation from the user's adb and its process
//! tracking, against a fake SDK: an `adb` that logs each call with the
//! server port it targets, and an `emulator` that starts a helper in its
//! process group (as the real one starts netsimd) and exits on `emu kill`
//! without it.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread::sleep;
use std::time::{Duration, Instant};

use tempfile::TempDir;

const SCRIPT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/emulator");

const FAKE_ADB: &str = r#"#!/bin/sh
echo "${ANDROID_ADB_SERVER_PORT:-5037} $*" >> "$FAKE_SDK/adb.log"
[ "$1" = -s ] && shift 2
case "$*" in
  "start-server"|"kill-server") ;;
  "shell getprop sys.boot_completed") echo 1 ;;
  "emu kill") kill "$(cat "$FAKE_SDK/$ANDROID_SERIAL.pid")" ;;
  "exec-out cat /sdcard/pm-ui.xml") cat "$FAKE_SDK/ui.xml" ;;
  shell*) ;;
  *) echo "fake adb: unexpected: $*" >&2; exit 1 ;;
esac
"#;

const FAKE_EMULATOR: &str = r#"#!/bin/sh
while [ "$1" != -port ]; do shift; done
serial=emulator-$2
echo "$TMPDIR" > "$FAKE_SDK/$serial.tmpdir"
sleep 600 &
echo $! > "$FAKE_SDK/$serial.helper"
echo $$ > "$FAKE_SDK/$serial.pid"
exec sleep 600
"#;

struct Sdk {
    tmp: TempDir,
}

impl Sdk {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let sdk = tmp.path().join("sdk");
        let image = sdk.join("system-images/android-37.0/google_apis/arm64-v8a");
        fs::create_dir_all(&image).unwrap();
        fs::write(image.join("system.img"), "").unwrap();
        for (path, body) in [
            ("platform-tools/adb", FAKE_ADB),
            ("emulator/emulator", FAKE_EMULATOR),
        ] {
            let file = sdk.join(path);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(&file, body).unwrap();
            Command::new("chmod").arg("+x").arg(&file).status().unwrap();
        }
        Sdk { tmp }
    }

    fn sdk(&self) -> PathBuf {
        self.tmp.path().join("sdk")
    }

    fn state(&self) -> PathBuf {
        self.tmp.path().join("state")
    }

    /// Run the script as on studio, whose shell points adb at the
    /// forwarded server on 5038.
    fn output(&self, args: &[&str]) -> Output {
        Command::new("bash")
            .arg(SCRIPT)
            .args(args)
            .env("ANDROID_HOME", self.sdk())
            .env("FAKE_SDK", self.sdk())
            .env("PM_EMULATOR_STATE", self.state())
            .env("ANDROID_ADB_SERVER_PORT", "5038")
            .env("ANDROID_SERIAL", "PHONE123")
            .output()
            .expect("spawn scripts/emulator")
    }

    fn run(&self, args: &[&str]) -> Output {
        let out = self.output(args);
        assert!(
            out.status.success(),
            "scripts/emulator {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }

    fn record(&self, name: &str, key: &str) -> String {
        let text = fs::read_to_string(self.state().join(name).join("instance")).unwrap();
        text.lines()
            .find_map(|l| l.strip_prefix(&format!("{key}=")))
            .unwrap_or_else(|| panic!("no {key} in {name}'s record"))
            .to_string()
    }

    fn pid(&self, file: &str) -> String {
        fs::read_to_string(self.sdk().join(file))
            .unwrap()
            .trim()
            .to_string()
    }

    fn adb_calls(&self) -> Vec<(String, String)> {
        fs::read_to_string(self.sdk().join("adb.log"))
            .unwrap_or_default()
            .lines()
            .map(|l| {
                let (port, args) = l.split_once(' ').unwrap();
                (port.to_string(), args.to_string())
            })
            .collect()
    }
}

/// Every fake process still running, killed even when a test fails.
impl Drop for Sdk {
    fn drop(&mut self) {
        for entry in fs::read_dir(self.sdk()).into_iter().flatten().flatten() {
            let path = entry.path();
            if matches!(
                path.extension().and_then(|e| e.to_str()),
                Some("pid" | "helper")
            ) {
                let _ = Command::new("kill")
                    .arg(fs::read_to_string(&path).unwrap_or_default().trim())
                    .output();
            }
        }
    }
}

fn alive(pid: &str) -> bool {
    Command::new("kill")
        .args(["-0", pid])
        .output()
        .unwrap()
        .status
        .success()
}

fn wait_dead(pid: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while alive(pid) {
        assert!(Instant::now() < deadline, "pid {pid} still running");
        sleep(Duration::from_millis(50));
    }
}

fn under(path: &str, dir: &Path) -> bool {
    Path::new(path).starts_with(fs::canonicalize(dir).unwrap()) || Path::new(path).starts_with(dir)
}

#[test]
fn instances_use_their_own_adb_servers_and_down_stops_their_helpers() {
    let sdk = Sdk::new();
    sdk.run(&["-n", "a", "up"]);
    sdk.run(&["-n", "b", "up"]);

    let (a_port, b_port) = (sdk.record("a", "adb_port"), sdk.record("b", "adb_port"));
    let (a_serial, b_serial) = (sdk.record("a", "serial"), sdk.record("b", "serial"));
    assert_ne!(a_port, b_port);
    assert_ne!(a_serial, b_serial);
    for (port, args) in sdk.adb_calls() {
        assert!(
            port == a_port || port == b_port,
            "adb `{args}` targeted server port {port}"
        );
    }
    for serial in [&a_serial, &b_serial] {
        let tmpdir = fs::read_to_string(sdk.sdk().join(format!("{serial}.tmpdir"))).unwrap();
        assert!(
            under(tmpdir.trim(), &sdk.state()),
            "{serial} shares TMPDIR {tmpdir}"
        );
    }

    let a_helper = sdk.pid(&format!("{a_serial}.helper"));
    sdk.run(&["-n", "a", "down"]);
    let b_server_stopped = (b_port.clone(), "kill-server".to_string());
    assert!(
        !sdk.adb_calls().contains(&b_server_stopped),
        "a's down stopped b's adb server"
    );
    wait_dead(&sdk.pid(&format!("{a_serial}.pid")));
    wait_dead(&a_helper);
    assert!(
        sdk.adb_calls()
            .contains(&(a_port.clone(), "kill-server".to_string())),
        "a's adb server was not stopped"
    );
    assert!(alive(&sdk.pid(&format!("{b_serial}.pid"))));
    assert!(alive(&sdk.pid(&format!("{b_serial}.helper"))));

    sdk.run(&["-n", "b", "down"]);
    wait_dead(&sdk.pid(&format!("{b_serial}.helper")));
    for (port, args) in sdk.adb_calls() {
        assert!(
            port == a_port || port == b_port,
            "adb `{args}` targeted server port {port}"
        );
    }
}

#[test]
fn down_leaves_alone_a_process_reusing_a_stale_records_pid() {
    let sdk = Sdk::new();
    let mut stranger = Command::new("perl")
        .args(["-MPOSIX=setsid", "-e", "setsid; exec @ARGV", "sleep", "600"])
        .spawn()
        .unwrap();
    let pid = stranger.id().to_string();
    let dir = sdk.state().join("crashed");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("instance"),
        format!(
            "pid={pid}\nstarted=Thu Jan  1 00:00:00 1970\nconsole=5554\nadb_port=5040\nserial=emulator-5554\n"
        ),
    )
    .unwrap();

    sdk.run(&["-n", "crashed", "down"]);
    sleep(Duration::from_millis(200));
    let survived = stranger.try_wait().unwrap().is_none();
    let _ = stranger.kill();
    let _ = stranger.wait();
    assert!(survived, "down signalled an unrelated process group");
    assert!(!dir.join("instance").exists());
}

const UI_DUMP: &str = r#"<?xml version='1.0' encoding='UTF-8' standalone='yes' ?><hierarchy rotation="0">
<node index="0" text="Pair" class="android.widget.TextView" content-desc="" bounds="[0,100][100,200]" />
<node index="1" text="" class="android.widget.EditText" content-desc="" bounds="[0,300][1000,400]" />
<node index="2" text="" class="android.view.View" content-desc="proj, 1 feature &amp; &lt;1&gt; waiting" bounds="[0,500][1000,700]" />
<node index="3" text="Pair" class="android.widget.TextView" content-desc="" bounds="[200,800][400,900]" />
</hierarchy>"#;

#[test]
fn tap_taps_the_one_matching_node_and_refuses_ambiguity() {
    let sdk = Sdk::new();
    fs::write(sdk.sdk().join("ui.xml"), UI_DUMP).unwrap();
    sdk.run(&["up"]);
    let taps = || -> Vec<String> {
        sdk.adb_calls()
            .into_iter()
            .filter_map(|(_, args)| args.strip_prefix("shell input tap ").map(str::to_string))
            .collect()
    };

    sdk.run(&["tap", "proj, 1 feature & <1> waiting"]);
    sdk.run(&["tap", "--class", "android.widget.EditText"]);
    assert_eq!(taps(), ["500 600", "500 350"]);

    let out = sdk.output(&["tap", "Pair"]);
    assert!(!out.status.success(), "an ambiguous tap succeeded");
    assert_eq!(taps().len(), 2);

    sdk.run(&["tap", "--nth", "2", "Pair"]);
    assert_eq!(taps().last().unwrap(), "300 850");
    assert!(
        !sdk.output(&["tap", "proj"]).status.success(),
        "a partial match tapped"
    );

    sdk.run(&["down"]);
}

#[test]
fn concurrent_ups_get_distinct_ports() {
    let sdk = Sdk::new();
    let ups: Vec<_> = ["a", "b", "c"]
        .iter()
        .map(|n| {
            Command::new("bash")
                .args([SCRIPT, "-n", n, "up"])
                .env("ANDROID_HOME", sdk.sdk())
                .env("FAKE_SDK", sdk.sdk())
                .env("PM_EMULATOR_STATE", sdk.state())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap()
        })
        .collect();
    for mut up in ups {
        assert!(up.wait().unwrap().success());
    }
    for key in ["console", "adb_port"] {
        let mut ports: Vec<_> = ["a", "b", "c"].map(|n| sdk.record(n, key)).to_vec();
        ports.sort();
        ports.dedup();
        assert_eq!(ports.len(), 3, "instances share a {key}");
    }
    for n in ["a", "b", "c"] {
        sdk.run(&["-n", n, "down"]);
    }
}
