use super::*;
use std::os::unix::fs::PermissionsExt;
use std::time::Instant;

/// A fake opencode in `dir`. As a server it records its arguments, password
/// and a child it leaves in its group, reports a URL (port 50123 for a
/// fresh one), and holds until its stdin closes — except the first, which
/// dies at once. As the TUI it records its arguments, password, address
/// file variable and whether that file existed, then exits 7 once a second
/// server started.
fn fake_opencode(dir: &Path) -> String {
    let bin = dir.join("opencode");
    crate::testing::write_executable(
        &bin,
        &format!(
            r#"#!/bin/sh
d='{d}'
if [ "$1" = serve ]; then
  n=1; while [ -e "$d/serve.$n" ]; do n=$((n+1)); done
  port=$4; [ "$port" = 0 ] && port=50123
  sleep 300 </dev/null >/dev/null 2>&1 &
  printf '%s\n' "$*" "$OPENCODE_PASSWORD" "$!" "$PM_OPENCODE_SERVER_FILE" > "$d/serve.$n"
  echo "{{\"url\":\"http://127.0.0.1:$port\"}}"
  [ "$n" = 1 ] && exit 1
  cat >/dev/null
  exit 0
fi
{{ printf '%s\n' "$*" "$OPENCODE_PASSWORD" "$PM_OPENCODE_SERVER_FILE"; [ -e "$d/{ADDRESS_FILE}" ] && echo endpoint; }} > "$d/tui.tmp"
mv "$d/tui.tmp" "$d/tui"
while [ ! -e "$d/serve.2" ]; do sleep 0.05; done
exit 7
"#,
            d = dir.display()
        ),
    );
    bin.to_string_lossy().into_owned()
}

fn lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect()
}

fn gone(pid: &str) -> bool {
    let pid: libc::pid_t = pid.parse().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        // SAFETY: probing a pid with signal 0 sends nothing.
        if unsafe { libc::kill(pid, 0) } != 0 {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

#[test]
fn the_host_serves_the_tui_restarts_its_server_on_the_same_port_and_ends_its_group() {
    let dir = tempfile::tempdir().unwrap();
    let host = OpenCodeHost {
        binary: fake_opencode(dir.path()),
        auto: true,
        session: Some("ses_1".into()),
    };
    let address_file = dir.path().join(ADDRESS_FILE);

    assert_eq!(serve(&host, &address_file, &|_| {}).unwrap(), 7);

    let file = address_file.to_string_lossy().into_owned();
    let first = lines(&dir.path().join("serve.1"));
    let second = lines(&dir.path().join("serve.2"));
    let tui = lines(&dir.path().join("tui"));
    assert_eq!(first[0], "serve --stdio --port 0");
    assert_eq!(second[0], "serve --stdio --port 50123");
    assert_eq!(
        tui,
        [
            "--server http://127.0.0.1:50123 --auto --session ses_1",
            &first[1],
            &file,
            "endpoint"
        ]
    );
    assert_eq!((&first[3], &second[3]), (&file, &file));
    let address = read_address(&address_file).unwrap();
    assert_eq!(address.url, "http://127.0.0.1:50123");
    assert_eq!(address.password.len(), 32);
    assert_eq!(
        (&first[1], &second[1]),
        (&address.password, &address.password)
    );
    let mode = std::fs::metadata(&address_file)
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);

    assert!(gone(&first[2]), "the dead server's group was left running");
    assert!(gone(&second[2]), "the server's group outlived the TUI");
}
