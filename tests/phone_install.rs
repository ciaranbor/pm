//! `scripts/phone install`, against a fake adb server on a real port, and a
//! fake `adb`, gradle wrapper and build-tools that log their calls.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

mod common;
use common::{AdbServer, closed_port};

const SCRIPT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/phone");
const PM_SIGNER: &str = "088ab70bc5d231494e285b9255ab6b73551abc23c0556e5ec0e5cee20123abec";
const APK: &str = "app/build/outputs/apk/google/release/app-google-release.apk";

struct Rig {
    tmp: TempDir,
}

impl Rig {
    fn new() -> Self {
        let rig = Rig {
            tmp: TempDir::new().unwrap(),
        };
        let log = rig.path("log");
        let log = log.display();
        rig.script(
            "bin/adb",
            &format!("echo \"adb $ANDROID_ADB_SERVER_PORT $*\" >> {log}"),
        );
        rig.script(
            "android/gradlew",
            &format!(
                "echo \"gradle $JAVA_HOME $*\" >> {log}\nmkdir -p \"$2/{dir}\"\ncat {signer} > \"$2/{APK}\"",
                dir = Path::new(APK).parent().unwrap().display(),
                signer = rig.path("signer").display(),
            ),
        );
        rig.script(
            "sdk/build-tools/37.0.0/apksigner",
            "echo \"Signer #1 certificate SHA-256 digest: $(cat \"$3\")\"",
        );
        rig.script(
            "sdk/build-tools/37.0.0/aapt2",
            "echo \"package: name='dev.pm.app' versionCode='42' versionName='0.3.0+1.gabc'\"",
        );
        fs::write(rig.path("signing.properties"), "").unwrap();
        fs::create_dir_all(rig.path("gradle")).unwrap();
        fs::write(
            rig.path("gradle/gradle.properties"),
            format!(
                "pmReleaseSigning={}\n",
                rig.path("signing.properties").display()
            ),
        )
        .unwrap();
        rig.sign_with(PM_SIGNER);
        rig
    }

    fn script(&self, name: &str, body: &str) {
        let path = self.path(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        Command::new("chmod").arg("+x").arg(&path).status().unwrap();
    }

    /// The certificate digest of the APKs the fake gradle builds.
    fn sign_with(&self, digest: &str) {
        fs::write(self.path("signer"), digest).unwrap();
    }

    fn path(&self, name: &str) -> PathBuf {
        self.tmp.path().join(name)
    }

    fn apk(&self) -> PathBuf {
        self.path("android").join(APK)
    }

    /// An APK left by an earlier build.
    fn built_apk(&self) {
        fs::create_dir_all(self.apk().parent().unwrap()).unwrap();
        fs::write(self.apk(), PM_SIGNER).unwrap();
    }

    fn run(&self, args: &[&str], env_port: Option<u16>) -> Output {
        self.command(args, env_port)
            .output()
            .expect("spawn scripts/phone")
    }

    fn command(&self, args: &[&str], env_port: Option<u16>) -> Command {
        let path = format!(
            "{}:{}",
            self.path("bin").display(),
            std::env::var("PATH").unwrap()
        );
        let mut cmd = Command::new("bash");
        cmd.arg(SCRIPT)
            .arg("install")
            .args(args)
            .env("PATH", path)
            .env("PM_PHONE_ANDROID", self.path("android"))
            .env("ANDROID_HOME", self.path("sdk"))
            .env("GRADLE_USER_HOME", self.path("gradle"))
            .env("JAVA_HOME", "/fake/jdk")
            .env_remove("ORG_GRADLE_PROJECT_pmReleaseSigning")
            .env_remove("ANDROID_SERIAL")
            .env_remove("ANDROID_ADB_SERVER_PORT");
        if let Some(port) = env_port {
            cmd.env("ANDROID_ADB_SERVER_PORT", port.to_string());
        }
        cmd
    }

    fn log(&self) -> String {
        fs::read_to_string(self.path("log")).unwrap_or_default()
    }

    fn installs(&self) -> Vec<String> {
        self.log()
            .lines()
            .filter(|l| l.starts_with("adb "))
            .map(String::from)
            .collect()
    }

    fn built(&self) -> bool {
        self.log().lines().any(|l| l.starts_with("gradle "))
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn ok(out: &Output) {
    assert!(out.status.success(), "install failed: {}", stderr(out));
}

#[test]
fn builds_the_release_and_installs_it_through_the_given_port() {
    let rig = Rig::new();
    let port = AdbServer::start("PHONE1\tdevice\n").port;
    let out = rig.run(&["--port", &port.to_string()], None);
    ok(&out);
    assert!(
        rig.log().contains("gradle /fake/jdk -p ") && rig.log().contains(" assembleGoogleRelease"),
        "{}",
        rig.log()
    );
    assert_eq!(
        rig.installs(),
        [format!(
            "adb {port} -s PHONE1 install -r {}",
            rig.apk().display()
        )]
    );
    assert!(stderr(&out).contains("versionName 0.3.0+1.gabc, versionCode 42"));
}

#[test]
fn the_port_flag_overrides_the_environment() {
    let rig = Rig::new();
    rig.built_apk();
    let port = AdbServer::start("PHONE1\tdevice\n").port;
    ok(&rig.run(
        &["--no-build", "--port", &port.to_string()],
        Some(closed_port()),
    ));
    ok(&rig.run(&["--no-build"], Some(port)));
    assert_eq!(
        rig.installs()
            .iter()
            .map(|l| l.split(' ').nth(1).unwrap())
            .collect::<Vec<_>>(),
        [port.to_string(), port.to_string()],
        "every install goes through the live server's port"
    );
}

#[test]
fn no_server_on_the_port_fails_without_running_adb_or_building() {
    let rig = Rig::new();
    let out = rig.run(&[], Some(closed_port()));
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("phone not connected"),
        "{}",
        stderr(&out)
    );
    assert_eq!(
        rig.log(),
        "",
        "adb would start a server; gradle wastes a build"
    );
}

#[test]
fn a_server_without_a_ready_device_fails_before_building() {
    let rig = Rig::new();
    for devices in ["", "PHONE1\tunauthorized\n"] {
        let port = AdbServer::start(devices).port;
        let out = rig.run(&["--port", &port.to_string()], None);
        assert!(!out.status.success());
        assert!(
            stderr(&out).contains("phone not connected: is the MacBook ssh session up?"),
            "{}",
            stderr(&out)
        );
    }
    assert_eq!(rig.log(), "");
}

#[test]
fn several_devices_need_android_serial() {
    let rig = Rig::new();
    let port = AdbServer::start("PHONE1\tdevice\nPHONE2\tdevice\n").port;
    let out = rig.run(&["--no-build", "--port", &port.to_string()], None);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("set ANDROID_SERIAL"),
        "{}",
        stderr(&out)
    );
    assert!(rig.installs().is_empty());
}

#[test]
fn android_serial_picks_one_of_several_devices() {
    let rig = Rig::new();
    rig.built_apk();
    let port = AdbServer::start("PHONE1\tdevice\nPHONE2\tdevice\n").port;
    let out = rig
        .command(&["--no-build"], Some(port))
        .env("ANDROID_SERIAL", "PHONE2")
        .output()
        .unwrap();
    ok(&out);
    assert_eq!(
        rig.installs(),
        [format!(
            "adb {port} -s PHONE2 install -r {}",
            rig.apk().display()
        )]
    );
}

#[test]
fn the_signing_property_resolves_as_gradle_does() {
    let rig = Rig::new();
    let port = AdbServer::start("PHONE1\tdevice\n").port;
    let properties = |value: &str| {
        fs::write(
            rig.path("gradle/gradle.properties"),
            format!("pmReleaseSigning={value}\n"),
        )
        .unwrap()
    };
    properties("/nonexistent/signing.properties");
    let out = rig
        .command(&[], Some(port))
        .env(
            "ORG_GRADLE_PROJECT_pmReleaseSigning",
            rig.path("signing.properties"),
        )
        .output()
        .unwrap();
    ok(&out);

    fs::create_dir_all(rig.path("android/app")).unwrap();
    fs::write(rig.path("android/app/relative.properties"), "").unwrap();
    properties("relative.properties");
    ok(&rig.run(&[], Some(port)));

    properties("~/relative.properties");
    let out = rig
        .command(&[], Some(port))
        .env("HOME", rig.path("android/app"))
        .output()
        .unwrap();
    assert!(stderr(&out).contains("not readable"), "{}", stderr(&out));
}

#[test]
fn unconfigured_signing_refuses_to_build() {
    let rig = Rig::new();
    fs::remove_file(rig.path("gradle/gradle.properties")).unwrap();
    let port = AdbServer::start("PHONE1\tdevice\n").port;
    let out = rig.run(&[], Some(port));
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("release signing is not configured"),
        "{}",
        stderr(&out)
    );
    assert!(!rig.built());
}

#[test]
fn an_apk_signed_with_another_key_is_not_installed() {
    let rig = Rig::new();
    rig.sign_with("0000000000000000000000000000000000000000000000000000000000000000");
    let port = AdbServer::start("PHONE1\tdevice\n").port;
    let out = rig.run(&[], Some(port));
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("not signed with pm's release key"),
        "{}",
        stderr(&out)
    );
    assert!(rig.installs().is_empty());
}

#[test]
fn no_build_installs_the_last_built_apk_without_building() {
    let rig = Rig::new();
    let port = AdbServer::start("PHONE1\tdevice\n").port;
    let out = rig.run(&["--no-build"], Some(port));
    assert!(!out.status.success(), "installed an APK nothing built");
    assert!(rig.installs().is_empty());

    rig.built_apk();
    ok(&rig.run(&["--no-build"], Some(port)));
    assert!(!rig.built());
    assert_eq!(rig.installs().len(), 1);
}
