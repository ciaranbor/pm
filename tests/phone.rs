//! `scripts/phone`'s holder bookkeeping, against a fake `adb` that keeps the
//! phone's `stay_on_while_plugged_in` in a file (absent = unset).

use std::fs;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use tempfile::TempDir;

const SCRIPT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/phone");

const FAKE_ADB: &str = r#"#!/bin/sh
f="$FAKE_PHONE/stay_on"
[ "$1" = -s ] && shift 2
case "$*" in
  get-serialno) echo FAKE123 ;;
  "shell settings get global stay_on_while_plugged_in") cat "$f" 2>/dev/null || echo null ;;
  "shell settings put global stay_on_while_plugged_in "*) echo "$6" > "$f" ;;
  "shell settings delete global stay_on_while_plugged_in") rm -f "$f" ;;
  "shell svc power stayon usb") echo 2 > "$f" ;;
  "shell input keyevent KEYCODE_WAKEUP") ;;
  "shell dumpsys window") echo "    isKeyguardShowing=$(cat "$FAKE_PHONE/locked" 2>/dev/null || echo false)" ;;
  *) echo "fake adb: unexpected: $*" >&2; exit 1 ;;
esac
"#;

struct Phone {
    tmp: TempDir,
}

impl Phone {
    fn new(stay_on: Option<&str>) -> Self {
        let tmp = TempDir::new().unwrap();
        let bin = tmp.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        fs::create_dir_all(tmp.path().join("phone")).unwrap();
        let adb = bin.join("adb");
        fs::write(&adb, FAKE_ADB).unwrap();
        Command::new("chmod").arg("+x").arg(&adb).status().unwrap();
        let phone = Phone { tmp };
        if let Some(v) = stay_on {
            fs::write(phone.stay_on_file(), format!("{v}\n")).unwrap();
        }
        phone
    }

    fn stay_on_file(&self) -> PathBuf {
        self.path("phone/stay_on")
    }

    fn stay_on(&self) -> Option<String> {
        fs::read_to_string(self.stay_on_file())
            .ok()
            .map(|s| s.trim().to_string())
    }

    fn command(&self, args: &[&str]) -> Command {
        let path = format!(
            "{}:{}",
            self.path("bin").display(),
            std::env::var("PATH").unwrap()
        );
        let mut cmd = Command::new("bash");
        cmd.arg(SCRIPT)
            .args(args)
            .env("PATH", path)
            .env("FAKE_PHONE", self.path("phone"))
            .env("PM_PHONE_STATE", self.path("state"))
            .env("PM_PHONE_WATCH_INTERVAL", "0.1")
            .env_remove("ANDROID_SERIAL");
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().expect("spawn scripts/phone")
    }

    /// Run FIRST at the start of every fake `adb` call.
    fn adb_prelude(&self, first: &str) {
        fs::write(
            self.path("bin/adb"),
            FAKE_ADB.replacen("#!/bin/sh\n", &format!("#!/bin/sh\n{first}\n"), 1),
        )
        .unwrap();
    }

    /// Make the fake `adb` fail every call whose arguments match PATTERN.
    fn fail_adb(&self, pattern: &str) {
        self.adb_prelude(&format!("case \"$*\" in {pattern}) exit 1 ;; esac"));
    }

    fn path(&self, name: &str) -> PathBuf {
        self.tmp.path().join(name)
    }

    /// Start `hold -- CMD` in its own process group, as a terminal would.
    fn spawn_wrapper(&self, cmd: &[&str]) -> Child {
        self.command(&[&["hold", "--"], cmd].concat())
            .process_group(0)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }

    fn holders(&self) -> usize {
        fs::read_dir(self.path("state/FAKE123/holders"))
            .map(|d| d.count())
            .unwrap_or(0)
    }
}

fn ok(out: &Output) {
    assert!(
        out.status.success(),
        "scripts/phone failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn sleeper() -> Child {
    Command::new("sleep").arg("60").spawn().unwrap()
}

fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        sleep(Duration::from_millis(50));
    }
}

#[test]
fn wrapper_holds_during_the_command_and_restores_after() {
    let phone = Phone::new(Some("0"));
    let seen = phone.path("seen");
    let out = phone.run(&[
        "hold",
        "--",
        "sh",
        "-c",
        &format!(
            "cat {} > {}",
            phone.stay_on_file().display(),
            seen.display()
        ),
    ]);
    ok(&out);
    assert_eq!(fs::read_to_string(&seen).unwrap().trim(), "2");
    assert_eq!(phone.stay_on().as_deref(), Some("0"));
    assert_eq!(phone.holders(), 0);
}

#[test]
fn wrapper_restores_and_propagates_a_failing_status() {
    let phone = Phone::new(Some("7"));
    let out = phone.run(&["hold", "--", "sh", "-c", "exit 3"]);
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(phone.stay_on().as_deref(), Some("7"));
}

#[test]
fn an_unset_setting_is_restored_as_unset() {
    let phone = Phone::new(None);
    ok(&phone.run(&["hold", "--", "true"]));
    assert_eq!(phone.stay_on(), None);
}

/// Ctrl-C at a terminal: SIGINT to the wrapper's whole process group.
fn ctrl_c(child: &Child) {
    Command::new("kill")
        .args(["-INT", "--", &format!("-{}", child.id())])
        .status()
        .unwrap();
}

#[test]
fn ctrl_c_releases() {
    let phone = Phone::new(Some("1"));
    let started = phone.path("started");
    let started_arg = started.display().to_string();
    let mut child = phone.spawn_wrapper(&[
        "perl",
        "-e",
        "open my $f, '>', shift or die; close $f; sleep 60",
        &started_arg,
    ]);
    wait_for("the command", || started.exists());
    ctrl_c(&child);
    assert_eq!(child.wait().unwrap().code(), Some(130));
    assert_eq!(phone.stay_on().as_deref(), Some("1"));
}

#[test]
fn ctrl_c_while_taking_the_hold_releases_without_running_the_command() {
    let phone = Phone::new(Some("1"));
    let (in_svc, go, ran) = (phone.path("in_svc"), phone.path("go"), phone.path("ran"));
    phone.adb_prelude(&format!(
        "case \"$*\" in *\" svc \"*) touch {}; i=0; while [ ! -e {} ] && [ $i -lt 200 ]; do sleep 0.05; i=$((i+1)); done ;; esac",
        in_svc.display(),
        go.display()
    ));
    let mut child = phone.spawn_wrapper(&["touch", &ran.display().to_string()]);
    wait_for("the hold to reach stay-on", || in_svc.exists());
    ctrl_c(&child);
    fs::write(&go, "").unwrap();
    assert_eq!(child.wait().unwrap().code(), Some(130));
    assert!(!ran.exists(), "the command ran after Ctrl-C");
    assert_eq!(phone.stay_on().as_deref(), Some("1"));
    assert_eq!(phone.holders(), 0);
}

#[test]
fn the_last_of_concurrent_holders_restores() {
    let phone = Phone::new(Some("0"));
    let mut a = sleeper();
    let a_pid = a.id().to_string();
    ok(&phone.run(&["hold", "--pid", &a_pid]));

    ok(&phone.run(&["hold", "--", "true"]));
    assert_eq!(phone.stay_on().as_deref(), Some("2"), "B released A's hold");

    ok(&phone.run(&["release", "--pid", &a_pid]));
    assert_eq!(phone.stay_on().as_deref(), Some("0"));
    a.kill().unwrap();
    a.wait().unwrap();
}

#[test]
fn a_dead_holder_is_dropped_and_the_setting_restored() {
    let phone = Phone::new(Some("0"));
    let mut a = sleeper();
    ok(&phone.run(&["hold", "--pid", &a.id().to_string()]));
    assert_eq!(phone.stay_on().as_deref(), Some("2"));

    a.kill().unwrap();
    a.wait().unwrap();
    wait_for("the watcher's release", || {
        phone.stay_on().as_deref() == Some("0")
    });
    assert_eq!(phone.holders(), 0);
}

#[test]
fn a_restore_that_failed_is_retried_by_the_next_run() {
    let phone = Phone::new(Some("0"));
    let mut a = sleeper();
    let a_pid = a.id().to_string();
    ok(&phone.run(&["hold", "--pid", &a_pid]));

    phone.fail_adb("*\" put \"*");
    assert!(!phone.run(&["release", "--pid", &a_pid]).status.success());
    assert_eq!(phone.stay_on().as_deref(), Some("2"));

    phone.fail_adb("never");
    ok(&phone.run(&["status"]));
    assert_eq!(phone.stay_on().as_deref(), Some("0"));
    a.kill().unwrap();
    a.wait().unwrap();
}

#[test]
fn only_a_locked_phone_asks_for_an_unlock() {
    let phone = Phone::new(Some("0"));
    let stderr = |phone: &Phone| {
        let out = phone.run(&["hold", "--", "true"]);
        ok(&out);
        String::from_utf8_lossy(&out.stderr).into_owned()
    };
    let unlocked = stderr(&phone);
    assert!(!unlocked.contains("is locked"), "{unlocked}");
    fs::write(phone.path("phone/locked"), "true").unwrap();
    let locked = stderr(&phone);
    assert!(locked.contains("is locked"), "{locked}");
}

#[test]
fn a_failed_stay_on_leaves_the_phone_and_state_as_they_were() {
    let phone = Phone::new(Some("0"));
    phone.fail_adb("*\" svc \"*");
    let out = phone.run(&["hold", "--", "true"]);
    assert!(!out.status.success());
    assert_eq!(phone.stay_on().as_deref(), Some("0"));
    assert_eq!(phone.holders(), 0);
    assert!(!phone.path("state/FAKE123/original").exists());
}

#[test]
fn release_stops_the_holders_watcher() {
    let phone = Phone::new(Some("0"));
    let mut a = sleeper();
    let a_pid = a.id().to_string();
    ok(&phone.run(&["hold", "--pid", &a_pid]));
    let record = phone.path("state/FAKE123/holders").join(&a_pid);
    let mut watcher = String::new();
    wait_for("the watcher to register", || {
        watcher = fs::read_to_string(&record)
            .unwrap_or_default()
            .lines()
            .nth(1)
            .unwrap_or_default()
            .to_string();
        !watcher.is_empty()
    });
    let alive = |pid: &str| {
        Command::new("kill")
            .args(["-0", pid])
            .status()
            .unwrap()
            .success()
    };
    assert!(alive(&watcher));

    ok(&phone.run(&["release", "--pid", &a_pid]));
    wait_for("the watcher to exit", || !alive(&watcher));
    assert_eq!(phone.stay_on().as_deref(), Some("0"));
    a.kill().unwrap();
    a.wait().unwrap();
}
