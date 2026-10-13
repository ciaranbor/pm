//! `pm update`: replace this binary with the latest GitHub release for its
//! target when that is newer, then run the upgrade steps
//! ([`upgrade`](super::upgrade)) with the new binary's `pm update --local`,
//! so the bundled assets installed are the new binary's, not this one's.
//! Without a newer release, or with `--local`, this binary runs them.
//!
//! The release is the one `releases/latest` names, which excludes
//! prereleases. Its `pm-<target>` asset is checked against the release's
//! `SHA256SUMS` before anything is written. The new binary is written
//! beside the old one and renamed over it, never written in place: on
//! macOS, overwriting a signed binary in place gets processes running it
//! killed, and the rename gives the new mtime `pm tmux watch` and `pm
//! serve` re-execute on ([`reexec`](super::reexec)).
//!
//! A build from source compares as the release it was built from, so a
//! newer release replaces it too.

use std::cmp::Ordering;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::Deserialize;

use crate::error::{PmError, Result};
use crate::hash::sha256_hex as sha256;
use crate::state::paths;
use crate::state::project::ProjectEntry;
use crate::version;

/// The GitHub API URL of pm's repository.
pub const REPO_API: &str = "https://api.github.com/repos/ciaranbor/pm";

/// The install script of the latest release.
pub const INSTALL_SCRIPT: &str =
    "https://github.com/ciaranbor/pm/releases/latest/download/install.sh";

/// Overrides [`REPO_API`], so a sandbox can serve releases of its own.
pub const REPO_API_ENV: &str = "PM_RELEASES_URL";

/// The largest asset downloaded.
const MAX_ASSET: u64 = 256 * 1024 * 1024;

/// What an update starts from and where it looks.
pub struct Update<'a> {
    /// The repository's API URL, as [`REPO_API`].
    pub api: &'a str,
    /// The binary to replace.
    pub exe: &'a Path,
    /// Its version.
    pub current: &'a str,
    /// Its target triple, naming the asset fetched.
    pub target: &'a str,
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

impl Release {
    fn asset(&self, name: &str) -> Result<&Asset> {
        self.assets.iter().find(|a| a.name == name).ok_or_else(|| {
            PmError::Update(format!("release {} has no asset {name}", self.tag_name))
        })
    }
}

/// An agent that gives up on a stalled server rather than waiting forever,
/// as ureq's defaults would.
fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .timeout_global(Some(Duration::from_secs(600)))
        .user_agent(format!("pm/{}", version::VERSION))
        .build()
        .into()
}

fn get(agent: &ureq::Agent, url: &str, accept: Option<&str>) -> Result<ureq::Body> {
    let mut request = agent.get(url);
    if let Some(accept) = accept {
        request = request.header("Accept", accept);
    }
    request
        .call()
        .map(|response| response.into_body())
        .map_err(|e| PmError::Update(format!("GET {url}: {e}")))
}

fn download(agent: &ureq::Agent, asset: &Asset) -> Result<Vec<u8>> {
    get(agent, &asset.browser_download_url, None)?
        .with_config()
        .limit(MAX_ASSET)
        .read_to_vec()
        .map_err(|e| PmError::Update(format!("downloading {}: {e}", asset.name)))
}

/// The digest `sums` (a `sha256sum` listing) gives for `name`.
fn listed_digest<'a>(sums: &'a str, name: &str) -> Option<&'a str> {
    sums.lines().find_map(|line| {
        let (digest, file) = line.split_once(char::is_whitespace)?;
        (file.trim_start().trim_start_matches('*') == name).then_some(digest)
    })
}

/// The release `api` names latest.
fn latest_release(agent: &ureq::Agent, api: &str) -> Result<Release> {
    let url = format!("{}/releases/latest", api.trim_end_matches('/'));
    let body = get(agent, &url, Some("application/vnd.github+json"))?;
    serde_json::from_reader(body.into_reader()).map_err(|e| PmError::Update(format!("{url}: {e}")))
}

/// Download `release`'s binary for `update.target`, check it against the
/// release's `SHA256SUMS` and install it over `update.exe`.
fn install_release(agent: &ureq::Agent, update: &Update<'_>, release: &Release) -> Result<()> {
    let name = format!("pm-{}", update.target);
    let binary = release.asset(&name).map_err(|_| {
        // The release workflow adds the macOS binary after the rest.
        let pending = if update.target.contains("apple-darwin") {
            "if it was published in the last hour, try again later; otherwise "
        } else {
            ""
        };
        PmError::Update(format!(
            "release {} has no binary for {}; {pending}build from source: \
             cargo install --git https://github.com/ciaranbor/pm",
            release.tag_name, update.target
        ))
    })?;
    let sums = download(agent, release.asset("SHA256SUMS")?)?;
    let sums = String::from_utf8_lossy(&sums);
    let expected = listed_digest(&sums, &name)
        .ok_or_else(|| PmError::Update(format!("SHA256SUMS lists no {name}")))?;
    let bytes = download(agent, binary)?;
    let actual = sha256(&bytes);
    if !actual.eq_ignore_ascii_case(expected) {
        return Err(PmError::Update(format!(
            "{name} does not match SHA256SUMS (expected {expected}, got {actual}); nothing was installed"
        )));
    }
    install(update.exe, &bytes)
}

/// What the release half of an update did.
#[derive(Debug)]
pub enum Outcome {
    /// Nothing was installed: the upgrade steps are this binary's to run,
    /// after the line saying why.
    Kept(String),
    /// A newer release was installed, and the line saying so: the upgrade
    /// steps are the new binary's to run.
    Installed(String),
}

/// Install the latest release over `update.exe` if it is newer than
/// `update.current`. A dry run only says whether it would. A release check
/// that fails is reported, not an error, so the upgrade steps still run; a
/// failed install is an error.
pub fn from_release(update: &Update<'_>, dry_run: bool) -> Result<Outcome> {
    let agent = agent();
    let release = match latest_release(&agent, update.api) {
        Ok(release) => release,
        Err(e) => {
            return Ok(Outcome::Kept(format!(
                "Warning: could not check for a newer pm release ({e}); upgrading with pm {}",
                update.current
            )));
        }
    };
    let latest = release.tag_name.trim_start_matches('v');
    if version::compare(latest, update.current) != Some(Ordering::Greater) {
        return Ok(Outcome::Kept(format!(
            "pm {} is up to date (latest release: {latest})",
            update.current
        )));
    }
    if dry_run {
        return Ok(Outcome::Kept(format!(
            "Would install pm {latest} over {}; the preview below is pm {}'s, not {latest}'s",
            update.current, update.current
        )));
    }
    install_release(&agent, update, &release).map_err(|e| {
        PmError::Update(format!(
            "{e} (`pm update --local` upgrades the projects without it)"
        ))
    })?;
    Ok(Outcome::Installed(format!(
        "Installed pm {latest} over {} ({})",
        update.current,
        update.exe.display()
    )))
}

/// Put `bytes` at `exe` by renaming a file written beside it.
fn install(exe: &Path, bytes: &[u8]) -> Result<()> {
    let dir = exe.parent().unwrap_or(Path::new("."));
    let cannot =
        |e: std::io::Error| PmError::Update(format!("cannot replace {}: {e}", exe.display()));
    let mut file = tempfile::Builder::new()
        .prefix(".pm-new-")
        .tempfile_in(dir)
        .map_err(cannot)?;
    file.write_all(bytes).map_err(cannot)?;
    file.as_file().sync_all().map_err(cannot)?;
    std::fs::set_permissions(file.path(), std::fs::Permissions::from_mode(0o755))
        .map_err(cannot)?;
    file.persist(exe).map_err(|e| cannot(e.error))?;
    Ok(())
}

/// Run `exe update --local`, its output going straight to this process's.
fn upgrade_with(exe: &Path) -> Result<()> {
    let status = run_just_written(Command::new(exe).args(["update", "--local"]))
        .map_err(|e| PmError::Update(format!("could not run `pm update --local`: {e}")))?;
    if !status.success() {
        return Err(PmError::Update(format!(
            "the new pm's `pm update --local` failed ({status})"
        )));
    }
    Ok(())
}

/// Run `command`, whose program was just written. On Linux its exec fails
/// with "Text file busy" while a child that another thread of this process
/// forked (as test threads do) still holds the write handle, until that
/// child execs.
fn run_just_written(command: &mut Command) -> std::io::Result<std::process::ExitStatus> {
    let mut tries = 0;
    loop {
        match command.status() {
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && tries < 20 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(50));
            }
            result => return result,
        }
    }
}

/// `pm update`: unless `local`, install a newer release first and have it
/// run the upgrade steps, its output printed as it goes; otherwise this
/// binary runs them.
pub fn update(local: bool, dry_run: bool, tmux_server: Option<&str>) -> Result<Vec<String>> {
    let mut lines = Vec::new();
    if !local {
        let api = std::env::var(REPO_API_ENV).unwrap_or_else(|_| REPO_API.to_string());
        let exe: PathBuf = std::env::current_exe()?.canonicalize()?;
        let outcome = from_release(
            &Update {
                api: &api,
                exe: &exe,
                current: version::VERSION,
                target: version::TARGET,
            },
            dry_run,
        )?;
        match outcome {
            Outcome::Kept(line) => lines.push(line),
            Outcome::Installed(line) => {
                println!("{line}");
                upgrade_with(&exe)?;
                return active_features_warning();
            }
        }
    }
    lines.extend(super::upgrade::upgrade(dry_run, tmux_server)?);
    Ok(lines)
}

/// A warning naming the projects with active features, which a new binary
/// may not suit; empty when there are none.
fn active_features_warning() -> Result<Vec<String>> {
    let active_features = count_active_features()?;
    if active_features.is_empty() {
        return Ok(Vec::new());
    }
    let total: usize = active_features.iter().map(|(_, c)| c).sum();
    let mut lines = vec![format!(
        "⚠ {total} active feature{} across {} project{} (new binary may differ from in-flight worktrees):",
        if total == 1 { "" } else { "s" },
        active_features.len(),
        if active_features.len() == 1 { "" } else { "s" },
    )];
    lines.extend(
        active_features
            .iter()
            .map(|(name, count)| format!("  {name}: {count}")),
    );
    Ok(lines)
}

/// Count active features across all registered projects.
/// Returns list of (project_name, count) pairs for projects with active features.
pub(crate) fn count_active_features() -> Result<Vec<(String, usize)>> {
    use crate::state::feature::{FeatureState, FeatureStatus};

    let projects_dir = paths::global_projects_dir()?;
    let projects = ProjectEntry::list(&projects_dir)?;
    let mut results = Vec::new();

    for (name, entry) in &projects {
        if !entry.presence().is_here() {
            continue;
        }
        let root = entry.root_path();
        let features_dir = paths::features_dir(&root);
        if let Ok(features) = FeatureState::list(&features_dir) {
            let active = features
                .iter()
                .filter(|(_, s)| !matches!(s.status, FeatureStatus::Merged | FeatureStatus::Stale))
                .count();
            if active > 0 {
                results.push((name.clone(), active));
            }
        }
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tempfile::{TempDir, tempdir};

    const TARGET: &str = "test-target";

    /// A stand-in for GitHub serving release `tag` with `binary` as its
    /// asset and `sums` as its SHA256SUMS; its API URL, and the number of
    /// asset downloads it has served.
    struct Releases {
        api: String,
        downloads: Arc<std::sync::atomic::AtomicUsize>,
        server: Arc<tiny_http::Server>,
    }

    impl Drop for Releases {
        fn drop(&mut self) {
            self.server.unblock();
        }
    }

    fn releases(tag: &str, binary: &[u8], sums: Option<String>) -> Releases {
        let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").unwrap());
        let base = format!("http://{}", server.server_addr().to_ip().unwrap());
        let asset = format!("pm-{TARGET}");
        let sums = sums.unwrap_or_else(|| format!("{}  {asset}\n", sha256(binary)));
        let latest = serde_json::json!({
            "tag_name": tag,
            "assets": [
                {"name": asset, "browser_download_url": format!("{base}/dl/{asset}")},
                {"name": "SHA256SUMS", "browser_download_url": format!("{base}/dl/SHA256SUMS")},
            ],
        })
        .to_string();
        let downloads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (serving, counted, binary) =
            (Arc::clone(&server), Arc::clone(&downloads), binary.to_vec());
        std::thread::spawn(move || {
            for request in serving.incoming_requests() {
                let body = match request.url() {
                    "/releases/latest" => latest.clone().into_bytes(),
                    "/dl/SHA256SUMS" => sums.clone().into_bytes(),
                    url if url == format!("/dl/pm-{TARGET}") => {
                        counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        binary.clone()
                    }
                    _ => {
                        let _ = request.respond(tiny_http::Response::empty(404));
                        continue;
                    }
                };
                let _ = request.respond(tiny_http::Response::from_data(body));
            }
        });
        Releases {
            api: base,
            downloads,
            server,
        }
    }

    /// An installed pm at `<dir>/bin/pm`, and a release binary that records
    /// the arguments it is run with in `<dir>/ran`.
    fn installed() -> (TempDir, PathBuf, Vec<u8>) {
        let dir = tempdir().unwrap();
        let exe = dir.path().join("bin/pm");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, "old").unwrap();
        let release = format!(
            "#!/bin/sh\necho \"$@\" > '{}'\n",
            dir.path().join("ran").display()
        );
        (dir, exe, release.into_bytes())
    }

    fn run(api: &str, exe: &Path, current: &str, dry_run: bool) -> Result<Outcome> {
        from_release(
            &Update {
                api,
                exe,
                current,
                target: TARGET,
            },
            dry_run,
        )
    }

    #[test]
    fn a_missing_binary_is_retried_later_only_on_macos() {
        let (_dir, exe, release) = installed();
        let github = releases("v0.3.0", &release, None);
        let missing = |target| {
            let result = from_release(
                &Update {
                    api: &github.api,
                    exe: &exe,
                    current: "0.2.0",
                    target,
                },
                false,
            );
            match result {
                Err(PmError::Update(message)) => message,
                other => panic!("{other:?}"),
            }
        };

        assert!(missing("aarch64-apple-darwin").contains("try again later"));
        assert!(!missing("x86_64-unknown-linux-gnu").contains("try again later"));
        assert_eq!(std::fs::read(&exe).unwrap(), b"old");
    }

    #[test]
    fn a_newer_release_is_verified_renamed_over_the_binary_and_upgrades_projects() {
        let (dir, exe, release) = installed();
        let github = releases("v0.3.0", &release, None);
        let outcome = run(&github.api, &exe, "0.2.0", false).unwrap();
        assert!(matches!(outcome, Outcome::Installed(_)), "{outcome:?}");
        assert_eq!(std::fs::read(&exe).unwrap(), release);
        assert_eq!(
            std::fs::metadata(&exe).unwrap().permissions().mode() & 0o777,
            0o755
        );
        upgrade_with(&exe).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("ran")).unwrap(),
            "update --local\n"
        );
        let leftovers: Vec<_> = std::fs::read_dir(exe.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, ["pm"]);
    }

    #[test]
    fn the_new_binarys_failed_upgrade_is_an_error() {
        let dir = tempdir().unwrap();
        let exe = dir.path().join("pm");
        install(&exe, b"#!/bin/sh\nexit 1\n").unwrap();
        let err = upgrade_with(&exe).unwrap_err();
        assert!(
            err.to_string().contains("`pm update --local` failed"),
            "{err}"
        );
    }

    #[test]
    fn a_binary_not_matching_its_checksum_is_not_installed() {
        let (dir, exe, release) = installed();
        let wrong = format!("{}  pm-{TARGET}\n", sha256(b"other"));
        let github = releases("v0.3.0", &release, Some(wrong));
        let err = run(&github.api, &exe, "0.2.0", false).unwrap_err();
        assert!(err.to_string().contains("SHA256SUMS"), "{err}");
        assert_eq!(std::fs::read_to_string(&exe).unwrap(), "old");
        assert!(!dir.path().join("ran").exists());
        assert_eq!(std::fs::read_dir(exe.parent().unwrap()).unwrap().count(), 1);
    }

    #[test]
    fn the_same_or_an_older_release_downloads_nothing() {
        let (_dir, exe, release) = installed();
        let github = releases("v0.2.0", &release, None);
        for current in ["0.2.0", "0.2.0+3.gabc1234", "0.3.0-rc.1", "1.0.0"] {
            let outcome = run(&github.api, &exe, current, false).unwrap();
            assert!(matches!(outcome, Outcome::Kept(_)), "{current}");
        }
        assert_eq!(
            github.downloads.load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        assert_eq!(std::fs::read_to_string(&exe).unwrap(), "old");
    }

    #[test]
    fn a_newer_release_replaces_a_build_from_source() {
        let (_dir, exe, release) = installed();
        let github = releases("v0.3.0", &release, None);
        let outcome = run(&github.api, &exe, "0.2.0+3.gabc1234", false).unwrap();
        assert!(matches!(outcome, Outcome::Installed(_)), "{outcome:?}");
        assert_eq!(std::fs::read(&exe).unwrap(), release);
    }

    #[test]
    fn a_dry_run_installs_nothing() {
        let (dir, exe, release) = installed();
        let github = releases("v0.3.0", &release, None);
        let Outcome::Kept(line) = run(&github.api, &exe, "0.2.0", true).unwrap() else {
            panic!("a dry run installed");
        };
        assert!(line.contains("Would install pm 0.3.0"), "{line}");
        assert_eq!(std::fs::read_to_string(&exe).unwrap(), "old");
        assert!(!dir.path().join("ran").exists());
    }

    #[test]
    fn an_unreachable_release_server_still_leaves_the_upgrade_steps_to_run() {
        let (_dir, exe, _) = installed();
        let unreachable = {
            let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
            format!("http://{}", server.server_addr().to_ip().unwrap())
        };
        let Outcome::Kept(line) = run(&unreachable, &exe, "0.2.0", false).unwrap() else {
            panic!("nothing to install");
        };
        assert!(line.starts_with("Warning: could not check"), "{line}");
        assert_eq!(std::fs::read_to_string(&exe).unwrap(), "old");
    }
}
