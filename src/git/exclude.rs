//! A repo's `info/exclude`: the ignore file every worktree of a clone
//! shares and git never commits. pm owns one block in it, between
//! [`BLOCK_START`] and [`BLOCK_END`], and leaves every other line alone.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::error::Result;

use super::run_git;

const BLOCK_START: &str =
    "# >>> pm: files pm writes into this repo's worktrees (rewritten by pm) >>>";
const BLOCK_END: &str = "# <<< pm <<<";

/// The `info/exclude` that `repo`'s worktrees share.
pub fn info_exclude_path(repo: &Path) -> Result<PathBuf> {
    let path = PathBuf::from(run_git(repo, &["rev-parse", "--git-path", "info/exclude"])?);
    Ok(if path.is_absolute() {
        path
    } else {
        repo.join(path)
    })
}

/// The files under `prefixes` (relative to `worktree`) that `worktree`'s
/// index tracks.
pub fn tracked_under(worktree: &Path, prefixes: &[&str]) -> Result<BTreeSet<PathBuf>> {
    let specs: Vec<String> = prefixes.iter().map(|p| format!(":(literal){p}")).collect();
    let mut args = vec!["ls-files", "-z", "--"];
    args.extend(specs.iter().map(String::as_str));
    Ok(run_git(worktree, &args)?
        .split('\0')
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .collect())
}

/// The repo-relative paths pm's block of `exclude` lists.
pub fn excluded_paths(exclude: &Path) -> Result<BTreeSet<PathBuf>> {
    let content = read(exclude)?;
    Ok(split(&content)
        .1
        .iter()
        .filter_map(|line| unescape(line))
        .collect())
}

/// Make pm's block of `exclude` list exactly `paths` (repo-relative files),
/// dropping the block when `paths` is empty. Whether the file changed.
pub fn write_excluded_paths(exclude: &Path, paths: &BTreeSet<PathBuf>) -> Result<bool> {
    let content = read(exclude)?;
    let (before, inside, after) = split(&content);
    let block: Vec<String> = paths.iter().filter_map(|p| escape(p)).collect();
    let has_block = content.lines().any(|l| l == BLOCK_START);
    if has_block == !block.is_empty() && inside == block {
        return Ok(false);
    }
    let mut lines: Vec<String> = before.iter().map(|l| l.to_string()).collect();
    if !block.is_empty() {
        lines.push(BLOCK_START.to_string());
        lines.extend(block);
        lines.push(BLOCK_END.to_string());
    }
    lines.extend(after.iter().map(|l| l.to_string()));
    let mut updated = lines.join("\n");
    if !updated.is_empty() {
        updated.push('\n');
    }
    if let Some(dir) = exclude.parent() {
        std::fs::create_dir_all(dir)?;
    }
    crate::fs_utils::write_atomic(exclude, updated.as_bytes())?;
    Ok(true)
}

fn read(exclude: &Path) -> Result<String> {
    match std::fs::read_to_string(exclude) {
        Ok(s) => Ok(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e.into()),
    }
}

/// The lines before pm's block, inside it, and after it. An unterminated
/// block runs to the end of the file.
fn split(content: &str) -> (Vec<&str>, Vec<&str>, Vec<&str>) {
    let lines: Vec<&str> = content.lines().collect();
    let Some(start) = lines.iter().position(|l| *l == BLOCK_START) else {
        return (lines, Vec::new(), Vec::new());
    };
    let end = lines[start..]
        .iter()
        .position(|l| *l == BLOCK_END)
        .map_or(lines.len(), |i| start + i);
    let after = lines.get(end + 1..).unwrap_or_default().to_vec();
    (
        lines[..start].to_vec(),
        lines[start + 1..end].to_vec(),
        after,
    )
}

/// `path` as an anchored gitignore pattern matching only itself; `None`
/// for a path no pattern line can hold.
fn escape(path: &Path) -> Option<String> {
    let s = path.to_str()?;
    if s.is_empty() || s.contains(['\n', '\r']) {
        return None;
    }
    let mut out = String::from("/");
    for c in s.chars() {
        if matches!(c, '\\' | '*' | '?' | '[' | '!' | '#') {
            out.push('\\');
        }
        out.push(c);
    }
    if out.ends_with(' ') {
        out.insert(out.len() - 1, '\\');
    }
    Some(out)
}

fn unescape(line: &str) -> Option<PathBuf> {
    let rest = line.strip_prefix('/')?;
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        out.push(if c == '\\' { chars.next()? } else { c });
    }
    Some(PathBuf::from(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(paths: &[&str]) -> BTreeSet<PathBuf> {
        paths.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn block_is_rewritten_in_place_and_other_lines_kept() {
        let dir = tempfile::tempdir().unwrap();
        let exclude = dir.path().join("info/exclude");
        std::fs::create_dir_all(exclude.parent().unwrap()).unwrap();
        std::fs::write(&exclude, "# git's template\n*.log\n").unwrap();

        assert!(write_excluded_paths(&exclude, &set(&["a/b.md", "c.json"])).unwrap());
        std::fs::write(
            &exclude,
            std::fs::read_to_string(&exclude).unwrap() + "mine\n",
        )
        .unwrap();
        assert!(!write_excluded_paths(&exclude, &set(&["a/b.md", "c.json"])).unwrap());
        assert!(write_excluded_paths(&exclude, &set(&["c.json"])).unwrap());

        let content = std::fs::read_to_string(&exclude).unwrap();
        assert_eq!(
            content,
            format!("# git's template\n*.log\n{BLOCK_START}\n/c.json\n{BLOCK_END}\nmine\n")
        );
        assert_eq!(excluded_paths(&exclude).unwrap(), set(&["c.json"]));

        assert!(write_excluded_paths(&exclude, &BTreeSet::new()).unwrap());
        assert_eq!(
            std::fs::read_to_string(&exclude).unwrap(),
            "# git's template\n*.log\nmine\n"
        );

        std::fs::write(
            &exclude,
            format!("{BLOCK_START}\n/c.json\n{BLOCK_END}\nlast"),
        )
        .unwrap();
        assert!(!write_excluded_paths(&exclude, &set(&["c.json"])).unwrap());
    }

    #[test]
    fn special_characters_match_only_the_literal_path() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        super::super::init_repo(&repo).unwrap();
        let odd = "skills/[x]*?#!/a b ";
        std::fs::create_dir_all(repo.join("skills/[x]*?#!")).unwrap();
        std::fs::write(repo.join(odd), "").unwrap();
        std::fs::write(repo.join("skills/x"), "").unwrap();
        let exclude = info_exclude_path(&repo).unwrap();

        write_excluded_paths(&exclude, &set(&[odd])).unwrap();

        assert_eq!(excluded_paths(&exclude).unwrap(), set(&[odd]));
        let untracked = super::super::untracked_files(&repo).unwrap();
        assert_eq!(untracked, vec!["skills/x".to_string()]);
    }
}
