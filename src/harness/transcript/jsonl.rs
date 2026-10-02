//! The chat view of a JSONL transcript, read by byte offset: a page backwards
//! from a cursor, or what was appended after one. A cursor is the offset of
//! a line's start, so a last line still being written is never read until
//! its newline lands. No read covers the whole file: a page reads back only
//! as far as it needs, a tail only what was appended, each up to a cap.
//!
//! A harness's parser turns one line into [`Entry`]s; this module pairs a
//! tool call with its result, which arrives on a later line. A page looks
//! forward past its end for the results of its calls; a tail looks back
//! before its start for the call of a result it holds alone, and sends the
//! call again with its result.

use std::collections::HashMap;
use std::io;
use std::path::Path;

use serde_json::Value;

use super::items::{Body, Item, Page, Tail, ToolResult};
use super::lines::{Backward, Source, open};

/// What one transcript line holds.
pub(in crate::harness) enum Entry {
    Item(Item),
    /// The output of the tool call whose item id is `call`.
    Result {
        call: String,
        text: String,
        error: bool,
    },
}

/// One line's entries, given the line and its offset (for an id when the
/// line carries none).
pub(in crate::harness) type Parse = fn(&Value, u64) -> Vec<Entry>;

/// How far back a page reads before returning what it has.
const PAGE_SCAN: u64 = 16 * 1024 * 1024;
/// How far a tail, or a search for a call or a result, reads.
const SEARCH_SCAN: u64 = 8 * 1024 * 1024;
fn parse_line(offset: u64, line: &str, parse: Parse) -> Vec<Entry> {
    serde_json::from_str::<Value>(line)
        .map(|value| parse(&value, offset))
        .unwrap_or_default()
}

/// The reference [`full_result`] reads the result on the line at `offset`
/// back from.
fn result_ref(offset: u64, call: &str) -> String {
    format!("{offset}:{call}")
}

fn tool_result(offset: u64, call: &str, text: &str, error: bool) -> ToolResult {
    ToolResult::new(text, error, || result_ref(offset, call))
}

fn pending_call(item: &Item) -> Option<&str> {
    match &item.body {
        Body::Tool { result: None, .. } => Some(&item.id),
        _ => None,
    }
}

fn attach(item: &mut Item, result: ToolResult) {
    if let Body::Tool { result: slot, .. } = &mut item.body {
        *slot = Some(result);
    }
}

/// A result whose call was not among the lines read.
struct Orphan {
    offset: u64,
    call: String,
    text: String,
    error: bool,
}

/// The items of `lines`, in order, each tool call with its result when the
/// result is among them; and the results whose calls were not.
fn assemble(lines: Vec<(u64, Vec<Entry>)>) -> (Vec<Item>, Vec<Orphan>) {
    let mut items: Vec<Item> = Vec::new();
    let mut calls: HashMap<String, usize> = HashMap::new();
    let mut orphans = Vec::new();
    for (offset, entries) in lines {
        for entry in entries {
            match entry {
                Entry::Item(item) => {
                    if matches!(item.body, Body::Tool { .. }) {
                        calls.insert(item.id.clone(), items.len());
                    }
                    items.push(item);
                }
                Entry::Result { call, text, error } => match calls.get(&call) {
                    Some(&at) => attach(&mut items[at], tool_result(offset, &call, &text, error)),
                    None => orphans.push(Orphan {
                        offset,
                        call,
                        text,
                        error,
                    }),
                },
            }
        }
    }
    (items, orphans)
}

/// Up to about `limit` items ending at cursor `before` (the end when
/// `None`), oldest first.
pub(in crate::harness) fn page(
    path: &Path,
    before: Option<u64>,
    limit: usize,
    parse: Parse,
) -> io::Result<Page> {
    let mut src = open(path)?;
    let complete = src.complete_len()?;
    let end = match before {
        Some(b) if b <= complete && src.is_line_start(b)? => b,
        _ => complete,
    };
    let mut back = Backward::new(end, PAGE_SCAN);
    let mut lines = Vec::new();
    let mut count = 0;
    let mut start = end;
    while count < limit {
        let Some((offset, line)) = back.next(&mut src)? else {
            break;
        };
        let entries = parse_line(offset, &line, parse);
        count += entries
            .iter()
            .filter(|e| matches!(e, Entry::Item(_)))
            .count();
        start = offset;
        lines.push((offset, entries));
    }
    lines.reverse();
    let (mut items, _) = assemble(lines);

    let pending: Vec<String> = items
        .iter()
        .filter_map(pending_call)
        .map(str::to_string)
        .collect();
    if !pending.is_empty() {
        let (later, _) = src.forward(end, SEARCH_SCAN)?;
        let mut found: HashMap<String, ToolResult> = HashMap::new();
        for (offset, line) in later {
            if !pending.iter().any(|id| line.contains(id.as_str())) {
                continue;
            }
            for entry in parse_line(offset, &line, parse) {
                if let Entry::Result { call, text, error } = entry
                    && pending.contains(&call)
                {
                    let result = tool_result(offset, &call, &text, error);
                    found.insert(call, result);
                }
            }
            if found.len() == pending.len() {
                break;
            }
        }
        for item in &mut items {
            if let Some(result) = found.remove(&item.id) {
                attach(item, result);
            }
        }
    }
    Ok(Page {
        items,
        before: (start > 0).then(|| start.to_string()),
        after: complete.to_string(),
    })
}

/// The items appended after cursor `after`.
pub(in crate::harness) fn tail(path: &Path, after: u64, parse: Parse) -> io::Result<Tail> {
    let mut src = open(path)?;
    if after > src.len || !src.is_line_start(after)? {
        return Ok(Tail::Reset);
    }
    let (lines, end) = src.forward(after, SEARCH_SCAN)?;
    let lines = lines
        .into_iter()
        .map(|(offset, line)| (offset, parse_line(offset, &line, parse)))
        .collect();
    let (items, orphans) = assemble(lines);
    let mut calls = Vec::new();
    for orphan in orphans {
        if let Some(mut call) = find_call(&mut src, after, &orphan.call, parse)? {
            attach(
                &mut call,
                tool_result(orphan.offset, &orphan.call, &orphan.text, orphan.error),
            );
            calls.push(call);
        }
    }
    calls.extend(items);
    Ok(Tail::Items {
        items: calls,
        after: end.to_string(),
    })
}

/// The tool call item `id`, from a line before `before`.
fn find_call(src: &mut Source, before: u64, id: &str, parse: Parse) -> io::Result<Option<Item>> {
    let mut back = Backward::new(before, SEARCH_SCAN);
    while let Some((offset, line)) = back.next(src)? {
        if !line.contains(id) {
            continue;
        }
        let call = parse_line(offset, &line, parse)
            .into_iter()
            .find_map(|entry| match entry {
                Entry::Item(item) if item.id == id => Some(item),
                _ => None,
            });
        if call.is_some() {
            return Ok(call);
        }
    }
    Ok(None)
}

/// The whole output a truncated result's `full` reference names.
pub(in crate::harness) fn full_result(
    path: &Path,
    reference: &str,
    parse: Parse,
) -> io::Result<Option<String>> {
    let Some((offset, call)) = reference.split_once(':') else {
        return Ok(None);
    };
    let Ok(offset) = offset.parse::<u64>() else {
        return Ok(None);
    };
    let mut src = open(path)?;
    if offset >= src.len || !src.is_line_start(offset)? {
        return Ok(None);
    }
    let (lines, _) = src.forward(offset, 1)?;
    Ok(lines.into_iter().next().and_then(|(offset, line)| {
        parse_line(offset, &line, parse)
            .into_iter()
            .find_map(|entry| match entry {
                Entry::Result { call: c, text, .. } if c == call => Some(text),
                _ => None,
            })
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::claude_code::chat::parse;
    use crate::harness::transcript::items::RESULT_LIMIT;
    use crate::harness::transcript::lines::CHUNK;
    use crate::harness::transcript::testing::append;
    use serde_json::json;
    use tempfile::tempdir;

    fn typed(uuid: &str, text: &str) -> String {
        json!({"type": "user", "uuid": uuid, "promptSource": "typed",
               "timestamp": "2026-10-02T18:00:00Z",
               "message": {"role": "user", "content": text}})
        .to_string()
    }

    fn call(uuid: &str, id: &str) -> String {
        json!({"type": "assistant", "uuid": uuid,
               "message": {"content": [{"type": "tool_use", "id": id, "name": "Bash",
                                        "input": {"command": "ls"}}]}})
        .to_string()
    }

    fn result(uuid: &str, id: &str, output: &str) -> String {
        json!({"type": "user", "uuid": uuid,
               "message": {"content": [{"type": "tool_result", "tool_use_id": id,
                                        "content": output}]}})
        .to_string()
    }

    fn ids(items: &[Item]) -> Vec<&str> {
        items.iter().map(|i| i.id.as_str()).collect()
    }

    fn tool_result_of(item: &Item) -> Option<&ToolResult> {
        match &item.body {
            Body::Tool { result, .. } => result.as_ref(),
            _ => None,
        }
    }

    #[test]
    fn pages_read_backwards_join_into_the_whole_conversation() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let noise = r#"{"type":"attachment","attachment":{"type":"x"}}"#.to_string();
        let mut lines = Vec::new();
        for n in 0..7 {
            lines.push(typed(&format!("u{n}"), &format!("prompt {n}")));
            lines.push(noise.clone());
        }
        append(&path, &lines);

        let whole = page(&path, None, 100, parse).unwrap();
        assert_eq!(whole.items.len(), 7);
        assert_eq!(whole.before, None);
        let len = std::fs::metadata(&path).unwrap().len();
        assert_eq!(whole.after, len.to_string());

        let mut joined = Vec::new();
        let mut before = None;
        loop {
            let p = page(&path, before, 3, parse).unwrap();
            assert!(p.items.len() <= 3);
            joined.splice(0..0, p.items);
            match p.before {
                Some(b) => before = Some(b.parse().unwrap()),
                None => break,
            }
        }
        assert_eq!(ids(&joined), ids(&whole.items));
    }

    #[test]
    fn lines_longer_than_a_read_chunk_page_whole() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let lines: Vec<String> = (0..5)
            .map(|n| {
                typed(
                    &format!("u{n}"),
                    &n.to_string().repeat(CHUNK as usize / 2 * 3),
                )
            })
            .collect();
        append(&path, &lines);
        let mut before = None;
        let mut seen = Vec::new();
        loop {
            let p = page(&path, before, 1, parse).unwrap();
            for item in &p.items {
                let Body::User { text } = &item.body else {
                    panic!("{item:?}");
                };
                assert_eq!(text.len(), CHUNK as usize / 2 * 3, "{}", item.id);
            }
            seen.splice(0..0, p.items.into_iter().map(|i| i.id));
            match p.before {
                Some(b) => before = Some(b.parse().unwrap()),
                None => break,
            }
        }
        assert_eq!(seen, ["u0", "u1", "u2", "u3", "u4"]);
    }

    #[test]
    fn a_tail_waits_for_a_partly_written_line() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        append(&path, &[typed("u0", "first")]);
        let start = page(&path, None, 0, parse).unwrap().after;
        let line = typed("u1", "second");
        let (head, rest) = line.split_at(10);
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .and_then(|mut f| std::io::Write::write_all(&mut f, head.as_bytes()))
            .unwrap();

        let Tail::Items { items, after } = tail(&path, start.parse().unwrap(), parse).unwrap()
        else {
            panic!("reset");
        };
        assert!(items.is_empty());
        assert_eq!(after, start, "the cursor stays before the partial line");
        assert_eq!(
            page(&path, None, 10, parse).unwrap().items.len(),
            1,
            "a page ignores it too"
        );

        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .and_then(|mut f| std::io::Write::write_all(&mut f, format!("{rest}\n").as_bytes()))
            .unwrap();
        let Tail::Items { items, .. } = tail(&path, after.parse().unwrap(), parse).unwrap() else {
            panic!("reset");
        };
        assert_eq!(ids(&items), ["u1"]);
    }

    #[test]
    fn a_result_after_its_call_is_paired_across_reads() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        append(&path, &[typed("u0", "go"), call("a1", "toolu_1")]);
        let Tail::Items { items, after } = tail(&path, 0, parse).unwrap() else {
            panic!("reset");
        };
        assert_eq!(ids(&items), ["u0", "toolu_1"]);
        assert!(tool_result_of(&items[1]).is_none());

        append(
            &path,
            &[result("r1", "toolu_1", "listing"), typed("u2", "next")],
        );
        let Tail::Items { items, .. } = tail(&path, after.parse().unwrap(), parse).unwrap() else {
            panic!("reset");
        };
        assert_eq!(ids(&items), ["toolu_1", "u2"], "the call is sent again");
        assert_eq!(tool_result_of(&items[0]).unwrap().text, "listing");

        // A page ending between the call and its result looks past its end.
        let text = std::fs::read_to_string(&path).unwrap();
        let before = text.find(r#""uuid":"r1""#).unwrap();
        let before = text[..before].rfind('\n').unwrap() as u64 + 1;
        let earlier = page(&path, Some(before), 10, parse).unwrap();
        assert_eq!(ids(&earlier.items), ["u0", "toolu_1"]);
        assert_eq!(tool_result_of(&earlier.items[1]).unwrap().text, "listing");
    }

    #[test]
    fn a_cursor_that_is_not_a_line_start_resets() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        append(&path, &[typed("u0", "go")]);
        assert_eq!(tail(&path, 3, parse).unwrap(), Tail::Reset);
        assert_eq!(tail(&path, 10_000, parse).unwrap(), Tail::Reset);
    }

    #[test]
    fn a_long_result_is_cut_and_read_back_whole() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let output = "y".repeat(RESULT_LIMIT * 3);
        append(
            &path,
            &[call("a1", "toolu_1"), result("r1", "toolu_1", &output)],
        );
        let p = page(&path, None, 10, parse).unwrap();
        let result = tool_result_of(&p.items[0]).unwrap();
        assert!(result.truncated);
        assert_eq!(result.text.len(), RESULT_LIMIT);
        let full = result.full.as_deref().unwrap();
        assert_eq!(
            full_result(&path, full, parse).unwrap().as_deref(),
            Some(output.as_str())
        );
        assert_eq!(full_result(&path, "1:toolu_1", parse).unwrap(), None);
    }

    #[test]
    fn a_compressed_rollout_reads_like_the_plain_one() {
        let dir = tempdir().unwrap();
        let plain = dir.path().join("s.jsonl");
        append(
            &plain,
            &[
                typed("u0", "go"),
                call("a1", "toolu_1"),
                result("r1", "toolu_1", "ok"),
            ],
        );
        let compressed = dir.path().join("s.jsonl.zst");
        let bytes = std::fs::read(&plain).unwrap();
        let packed = ruzstd::encoding::compress_to_vec(
            bytes.as_slice(),
            ruzstd::encoding::CompressionLevel::Fastest,
        );
        std::fs::write(&compressed, packed).unwrap();
        assert_eq!(
            page(&compressed, None, 10, parse).unwrap(),
            page(&plain, None, 10, parse).unwrap()
        );
    }
}
