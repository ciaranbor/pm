//! The chat view of an opencode session, read from opencode's sqlite
//! database (`<data dir>/opencode.db`): one `session_message` row per
//! message, ordered by `seq`, its body JSON in `data` (verified on 2.0.18).
//!
//! The database is opened read-only and only ever read. It is reached
//! directly rather than through `opencode api`, which starts a server per
//! call (about 0.25 s) — too slow for a view polled every second.
//!
//! A row is rewritten while its message streams, moving its `time_updated`,
//! so a tail's cursor is the latest `time_updated` read, not a `seq`. Rows
//! written in the same millisecond as the cursor are read again by the next
//! tail; a reader drops what it has already sent.
//!
//! What each row reads as is [`messages`](super::messages).

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags, params};
use serde_json::Value;

use crate::error::{PmError, Result};
use crate::harness::transcript::items::{Page, Tail};

use super::messages::{Row, items, tool_output};

/// The database under `data_home` (`$XDG_DATA_HOME`), else under
/// `<home>/.local/share`.
pub(in crate::harness) fn db_path(home: &Path, data_home: Option<&Path>) -> PathBuf {
    data_home
        .map_or_else(|| home.join(".local/share"), Path::to_path_buf)
        .join("opencode/opencode.db")
}

fn open(db: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(sql)?;
    conn.busy_timeout(std::time::Duration::from_secs(2))
        .map_err(sql)?;
    Ok(conn)
}

fn sql(e: rusqlite::Error) -> PmError {
    PmError::Transcript(format!("opencode database: {e}"))
}

/// Rows read per query of a page, which may give no items each.
const BATCH: i64 = 50;

const COLUMNS: &str = "id, type, seq, time_created, time_updated, data";

fn rows(conn: &Connection, query: &str, args: impl rusqlite::Params) -> Result<Vec<Row>> {
    let mut stmt = conn.prepare(query).map_err(sql)?;
    let rows = stmt
        .query_map(args, |r| {
            Ok(Row {
                id: r.get(0)?,
                kind: r.get(1)?,
                seq: r.get(2)?,
                created: r.get(3)?,
                updated: r.get(4)?,
                data: r.get(5)?,
            })
        })
        .map_err(sql)?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(sql)
}

fn latest_update(conn: &Connection, session: &str) -> Result<i64> {
    conn.query_row(
        "SELECT COALESCE(MAX(time_updated), 0) FROM session_message WHERE session_id = ?1",
        params![session],
        |r| r.get(0),
    )
    .map_err(sql)
}

/// Up to about `limit` items of `session` before the row with seq
/// `before` (the end when `None`), oldest first.
pub(in crate::harness) fn page(
    db: &Path,
    session: &str,
    before: Option<i64>,
    limit: usize,
) -> Result<Page> {
    let conn = open(db)?;
    let after = latest_update(&conn, session)?;
    let query = format!(
        "SELECT {COLUMNS} FROM session_message WHERE session_id = ?1 AND seq < ?2 \
         ORDER BY seq DESC LIMIT ?3"
    );
    let mut cursor = before.unwrap_or(i64::MAX);
    let mut read: Vec<Row> = Vec::new();
    let mut count = 0;
    'read: while count < limit {
        let batch = rows(&conn, &query, params![session, cursor, BATCH])?;
        let exhausted = batch.len() < BATCH as usize;
        for row in batch {
            cursor = row.seq;
            count += items(&row).len();
            read.push(row);
            if count >= limit {
                break 'read;
            }
        }
        if exhausted {
            break;
        }
    }
    read.reverse();
    let before = match read.first() {
        Some(first) if has_before(&conn, session, first.seq)? => Some(first.seq.to_string()),
        _ => None,
    };
    Ok(Page {
        items: read.iter().flat_map(items).collect(),
        before,
        after: after.to_string(),
    })
}

fn has_before(conn: &Connection, session: &str, seq: i64) -> Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM session_message WHERE session_id = ?1 AND seq < ?2)",
        params![session, seq],
        |r| r.get(0),
    )
    .map_err(sql)
}

/// The items of `session`'s rows written or rewritten at or after
/// `after`, a `time_updated`.
pub(in crate::harness) fn tail(db: &Path, session: &str, after: i64) -> Result<Tail> {
    let conn = open(db)?;
    let query = format!(
        "SELECT {COLUMNS} FROM session_message WHERE session_id = ?1 AND time_updated >= ?2 \
         ORDER BY seq"
    );
    let read = rows(&conn, &query, params![session, after])?;
    let latest = read.iter().map(|r| r.updated).max().unwrap_or(after);
    Ok(Tail::Items {
        items: read.iter().flat_map(items).collect(),
        after: latest.to_string(),
    })
}

/// The whole output a truncated result's `full` reference names.
pub(in crate::harness) fn full_result(
    db: &Path,
    session: &str,
    reference: &str,
) -> Result<Option<String>> {
    let Some((row, part)) = reference.rsplit_once(':') else {
        return Ok(None);
    };
    let Ok(part) = part.parse::<usize>() else {
        return Ok(None);
    };
    let conn = open(db)?;
    let data: Option<String> = conn
        .query_row(
            "SELECT data FROM session_message WHERE session_id = ?1 AND id = ?2",
            params![session, row],
            |r| r.get(0),
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            e => Err(sql(e)),
        })?;
    let Some(data) = data.and_then(|d| serde_json::from_str::<Value>(&d).ok()) else {
        return Ok(None);
    };
    Ok(data
        .pointer(&format!("/content/{part}/state"))
        .map(|state| tool_output(state).0))
}

#[cfg(test)]
pub(crate) mod testing {
    use std::path::{Path, PathBuf};

    use rusqlite::{Connection, params};
    use serde_json::Value;

    /// opencode 2.0.18's `session_message` table.
    const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS `session_message` (
          `id` text PRIMARY KEY,
          `session_id` text NOT NULL,
          `type` text NOT NULL,
          `seq` integer NOT NULL,
          `time_created` integer NOT NULL,
          `time_updated` integer NOT NULL,
          `data` text NOT NULL
        );
        CREATE UNIQUE INDEX IF NOT EXISTS `session_message_session_seq_idx` ON `session_message` (`session_id`,`seq`);";

    /// Where opencode keeps its database under `home`.
    pub fn db(home: &Path) -> PathBuf {
        super::db_path(home, None)
    }

    /// A database at `path` with opencode's message table.
    pub fn create(path: &Path) -> Connection {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        conn
    }

    /// Write a row as opencode does, created and updated at `updated`.
    pub fn insert(
        conn: &Connection,
        session: &str,
        id: &str,
        kind: &str,
        seq: i64,
        updated: i64,
        data: &Value,
    ) {
        conn.execute(
            "INSERT OR REPLACE INTO session_message VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![id, session, kind, seq, updated, updated, data.to_string()],
        )
        .unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::transcript::items::RESULT_LIMIT;
    use crate::harness::transcript::items::{Body, Item};
    use tempfile::{TempDir, tempdir};

    const FIXTURE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/transcripts/opencode.jsonl"
    ));
    const SESSION: &str = "ses_fixture";

    struct Db {
        _dir: TempDir,
        path: PathBuf,
        conn: Connection,
    }

    impl Db {
        fn new() -> Self {
            let dir = tempdir().unwrap();
            let path = dir.path().join("opencode.db");
            let conn = testing::create(&path);
            Self {
                _dir: dir,
                path,
                conn,
            }
        }

        fn with_fixture() -> Self {
            let db = Self::new();
            for line in FIXTURE.lines() {
                let row: Value = serde_json::from_str(line).unwrap();
                db.insert(
                    row["id"].as_str().unwrap(),
                    row["type"].as_str().unwrap(),
                    row["seq"].as_i64().unwrap(),
                    row["time_updated"].as_i64().unwrap(),
                    &row["data"],
                );
            }
            db
        }

        fn insert(&self, id: &str, kind: &str, seq: i64, updated: i64, data: &Value) {
            testing::insert(&self.conn, SESSION, id, kind, seq, updated, data);
        }
    }

    fn kinds(items: &[Item]) -> Vec<String> {
        items
            .iter()
            .map(|i| {
                serde_json::to_value(i).unwrap()["kind"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn real_rows_read_as_their_conversation() {
        let db = Db::with_fixture();
        let page = page(&db.path, SESSION, None, 100).unwrap();
        assert_eq!(
            kinds(&page.items),
            [
                "continuation", // the plugin's wake-up carries no `files`
                "thinking",
                "tool",
                "tool",
                "event", // a background shell's end
                "user",
                "compaction",
                "event", // an aborted step
                "thinking",
                "assistant",
                "tool",
                "event",
            ]
        );
        let Body::Tool {
            result: Some(result),
            ..
        } = &page.items[10].body
        else {
            panic!("{:?}", page.items[10]);
        };
        assert!(result.error, "an interrupted tool failed");
        assert_eq!(page.before, None);
    }

    #[test]
    fn pages_join_and_a_tail_rereads_rewritten_rows() {
        let db = Db::with_fixture();
        let whole = page(&db.path, SESSION, None, 100).unwrap();
        let mut joined = Vec::new();
        let mut before = None;
        loop {
            let p = page(&db.path, SESSION, before, 3).unwrap();
            joined.splice(0..0, p.items);
            match p.before {
                Some(b) => before = Some(b.parse().unwrap()),
                None => break,
            }
        }
        assert_eq!(joined, whole.items);

        let after: i64 = whole.after.parse().unwrap();
        let streaming = serde_json::json!({"time": {"created": after + 1},
            "content": [{"type": "text", "text": "Work"}]});
        db.insert("msg_new", "assistant", 50, after + 1, &streaming);
        let Tail::Items { items, after } = tail(&db.path, SESSION, after + 1).unwrap() else {
            panic!("reset");
        };
        assert_eq!(kinds(&items), ["assistant"]);

        let done = serde_json::json!({"time": {"created": 1},
            "content": [{"type": "text", "text": "Work done"}]});
        db.insert(
            "msg_new",
            "assistant",
            50,
            after.parse::<i64>().unwrap() + 5,
            &done,
        );
        let Tail::Items { items, .. } = tail(&db.path, SESSION, after.parse().unwrap()).unwrap()
        else {
            panic!("reset");
        };
        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0].id, "msg_new:0",
            "a rewritten row keeps its items' ids"
        );
        assert_eq!(
            items[0].body,
            Body::Assistant {
                text: "Work done".into()
            }
        );
    }

    #[test]
    fn a_long_tool_output_is_read_back_whole() {
        let db = Db::new();
        let output = "z".repeat(RESULT_LIMIT + 10);
        let row = serde_json::json!({"time": {"created": 1}, "content": [
            {"type": "tool", "id": "call_1", "name": "shell",
             "time": {"created": 1, "completed": 2500},
             "state": {"status": "completed", "input": {"command": "yes"},
                       "content": [{"type": "text", "text": output}]}}]});
        db.insert("msg_1", "assistant", 1, 1, &row);
        let page = page(&db.path, SESSION, None, 10).unwrap();
        let Body::Tool {
            result: Some(result),
            input,
            ..
        } = &page.items[0].body
        else {
            panic!("{:?}", page.items);
        };
        assert_eq!(input, "yes");
        assert!(result.truncated);
        assert_eq!(result.at, chrono::DateTime::from_timestamp_millis(2500));
        let full = full_result(&db.path, SESSION, result.full.as_deref().unwrap()).unwrap();
        assert_eq!(full.as_deref(), Some(output.as_str()));
        assert_eq!(full_result(&db.path, "ses_other", "msg_1:0").unwrap(), None);
    }
}
