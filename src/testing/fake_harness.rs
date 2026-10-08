//! Stand-ins for the harness binaries, so a test spawn launches something
//! pm recognises as a harness without running a real one.

use std::sync::OnceLock;

use super::test_home;

/// Write a stand-in `opencode` into `dir` and return its path, for
/// `[harness.opencode] binary`. It records its arguments in `<dir>/argv`,
/// one per line, and the opencode config its environment names in
/// `<dir>/env` (written first, so a new `argv` implies its `env`), keeping
/// earlier invocations as `argv.1`, `argv.2`, …; then
/// it prints `answer` and exits with `exit` — enough to play `opencode
/// api`, `opencode --version`, and the TUI a window launches.
pub fn fake_opencode(dir: &std::path::Path, answer: &str, exit: i32) -> String {
    fake_opencode_sequence(dir, &[answer], exit)
}

/// A [`fake_opencode`] whose n-th invocation in `dir` prints the n-th of
/// `answers`, and every later one the last.
pub fn fake_opencode_sequence(dir: &std::path::Path, answers: &[&str], exit: i32) -> String {
    let scripted: Vec<(&str, i32)> = answers.iter().map(|answer| (*answer, exit)).collect();
    fake_opencode_scripted(dir, &scripted)
}

/// A [`fake_opencode_sequence`] whose n-th invocation also exits with the
/// n-th code.
pub fn fake_opencode_scripted(dir: &std::path::Path, answers: &[(&str, i32)]) -> String {
    let bin = dir.join("opencode");
    let answer_file = |name: &str| dir.join(format!("answer.{name}"));
    let exit_file = |name: &str| dir.join(format!("exit.{name}"));
    for stale in (1..).map(|n| n.to_string()) {
        let _ = std::fs::remove_file(exit_file(&stale));
        if std::fs::remove_file(answer_file(&stale)).is_err() {
            break;
        }
    }
    let (last, earlier) = answers.split_last().expect("at least one answer");
    let write = |name: &str, (answer, exit): &(&str, i32)| {
        std::fs::write(answer_file(name), format!("{answer}\n"))
            .expect("write fake opencode answer");
        std::fs::write(exit_file(name), exit.to_string()).expect("write fake opencode exit");
    };
    for (n, answer) in earlier.iter().enumerate() {
        write(&(n + 1).to_string(), answer);
    }
    write("last", last);
    let script = format!(
        "#!/bin/sh\nn=1; while [ -e '{log}'.$n ]; do n=$((n+1)); done\n\
         k=$n; [ -e '{log}' ] && mv '{log}' '{log}'.$n && k=$((n+1))\n\
         printf 'OPENCODE_CONFIG=%s\\nOPENCODE_CONFIG_CONTENT=%s\\n' \\\n\
         \"$OPENCODE_CONFIG\" \"$OPENCODE_CONFIG_CONTENT\" > '{env}.tmp' && mv '{env}.tmp' '{env}'\n\
         printf '%s\\n' \"$@\" > '{log}.tmp' && mv '{log}.tmp' '{log}'\n\
         a='{answer}'.$k; e='{exit}'.$k; [ -e \"$a\" ] || {{ a='{answer}'.last; e='{exit}'.last; }}\n\
         cat \"$a\"\nexit \"$(cat \"$e\")\"\n",
        log = dir.join("argv").display(),
        env = dir.join("env").display(),
        answer = dir.join("answer").display(),
        exit = dir.join("exit").display(),
    );
    super::write_executable(&bin, &script);
    bin.to_string_lossy().into_owned()
}

/// What the last [`fake_opencode`] invocation in `dir` was called with;
/// empty when it has not run.
pub fn fake_opencode_argv(dir: &std::path::Path) -> Vec<String> {
    read_lines(&dir.join("argv"))
}

/// Every [`fake_opencode`] invocation in `dir`, oldest first.
pub fn fake_opencode_calls(dir: &std::path::Path) -> Vec<Vec<String>> {
    let mut calls: Vec<Vec<String>> = (1..)
        .map(|n| dir.join(format!("argv.{n}")))
        .take_while(|path| path.exists())
        .map(|path| read_lines(&path))
        .collect();
    calls.extend(Some(fake_opencode_argv(dir)).filter(|last| !last.is_empty()));
    calls
}

fn read_lines(path: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .map(|text| text.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

/// A `claude` that is `sleep` under another name, in the test home: a
/// window running it has a Claude Code harness as far as pm can tell.
pub(crate) fn fake_claude() -> std::path::PathBuf {
    static FAKE: OnceLock<std::path::PathBuf> = OnceLock::new();
    FAKE.get_or_init(|| {
        let dir = test_home().join("fake-bin");
        std::fs::create_dir_all(&dir).expect("create fake-bin");
        let bin = dir.join("claude");
        let _ = std::os::unix::fs::symlink("/bin/sleep", &bin);
        bin
    })
    .clone()
}

/// The file in an agent's runtime dir that holds a [`fake_harness_path`]
/// harness before its session starts.
pub(crate) const HOLD_START: &str = "hold-start";

/// A directory of stand-ins for `claude` and `codex`, ahead of the rest of
/// `PATH` in every test server's windows, so a spawn launches one: each
/// stamps its session as started, as the harness's session-start hook
/// would ([`runtime::started_at`](crate::state::runtime::started_at)), then
/// runs as [`fake_claude`] does. One launched for an agent whose runtime
/// dir holds [`HOLD_START`] never starts its session.
fn fake_harness_path() -> &'static std::path::Path {
    static DIR: OnceLock<std::path::PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = test_home().join("fake-path");
        std::fs::create_dir_all(&dir).expect("create fake-path");
        for harness in [
            crate::harness::Harness::ClaudeCode,
            crate::harness::Harness::Codex,
        ] {
            let program = fake_harness_binary(harness, std::path::Path::new("/bin/sleep"));
            let name = program.file_name().unwrap().to_string_lossy().into_owned();
            let script = format!(
                "#!/bin/sh\nwt=$PM_AGENT_WORKTREE\n\
                 dir=\"${{wt%/*}}/.pm/runtime/${{wt##*/}}/$PM_AGENT_NAME\"\n\
                 [ -n \"$wt\" ] && [ -n \"$PM_AGENT_NAME\" ] && [ ! -e \"$dir/{HOLD_START}\" ] && \
                 touch \"$dir/started\"\n\
                 exec {} 999\n",
                program.display()
            );
            let path = dir.join(&name);
            super::write_executable(&path, &script);
        }
        dir
    })
}

/// The `PATH` of a test window: [`fake_harness_path`], then the test's own.
pub(crate) fn window_path() -> &'static str {
    static PATH: OnceLock<String> = OnceLock::new();
    PATH.get_or_init(|| {
        format!(
            "{}:{}",
            fake_harness_path().display(),
            std::env::var("PATH").unwrap_or_default()
        )
    })
}

/// `program` under the name of `harness`'s binary, so a pane running it
/// runs the harness.
pub(crate) fn fake_harness_binary(
    harness: crate::harness::Harness,
    program: &std::path::Path,
) -> std::path::PathBuf {
    let config = crate::state::project::HarnessConfig::default();
    let name = ["claude", "codex", "opencode"]
        .into_iter()
        .find(|name| harness.runs_as(name, &config))
        .expect("a harness binary name");
    let tag: String = program
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let dir = test_home().join(format!("fake-{tag}"));
    std::fs::create_dir_all(&dir).expect("create fake harness dir");
    let bin = dir.join(name);
    let _ = std::os::unix::fs::symlink(program, &bin);
    bin
}

/// A script that draws Claude Code's input box, leaves the cursor on its
/// prompt line, and execs [`fake_claude`].
pub(super) fn fake_claude_at_prompt() -> std::path::PathBuf {
    static FAKE: OnceLock<std::path::PathBuf> = OnceLock::new();
    FAKE.get_or_init(|| {
        let claude = fake_claude();
        let script = claude.with_file_name("claude-at-prompt");
        super::write_executable(
            &script,
            &format!(
                "#!/bin/sh\nclear\nprintf '──── agent ─\\n❯ \\n────────\\n\\033[2A\\033[3G'\n\
                 exec {} 999\n",
                claude.display()
            ),
        );
        script
    })
    .clone()
}
