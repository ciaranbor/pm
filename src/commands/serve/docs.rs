//! `/v1/projects/{project}/docs[/{filename}]`: the project's information
//! store, read-only. Only the files `categories.toml` lists are served, each
//! by its listed filename, so a request names a category and never a path:
//! nothing else under `.pm/` can be reached.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::state::paths;

use super::routes::{MARKDOWN, Reply, error, json};

#[derive(Deserialize)]
struct Categories {
    #[serde(default)]
    category: Vec<Category>,
}

#[derive(Deserialize)]
struct Category {
    filename: String,
    #[serde(default)]
    description: String,
}

#[derive(Serialize)]
struct Listed {
    filename: String,
    description: String,
    /// Bytes; 0 when the file doesn't exist yet.
    size: u64,
    modified: Option<DateTime<Utc>>,
}

/// The categories `categories.toml` lists, in its order, keeping only
/// those whose filename is a file directly in the docs dir; none when it
/// is missing.
fn categories(root: &Path) -> Result<Vec<Category>> {
    let path = paths::docs_dir(root).join("categories.toml");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let parsed: Categories = toml::from_str(&text)?;
    Ok(parsed
        .category
        .into_iter()
        .filter(|c| plain_name(&c.filename))
        .collect())
}

fn plain_name(name: &str) -> bool {
    !matches!(name, "" | "." | ".." | "categories.toml") && !name.contains(['/', '\\'])
}

/// `GET /v1/projects/{project}/docs`.
pub(super) fn list(root: &Path) -> Result<Reply> {
    let dir = paths::docs_dir(root);
    let listed: Vec<Listed> = categories(root)?
        .into_iter()
        .map(|c| {
            let meta = std::fs::metadata(dir.join(&c.filename)).ok();
            Listed {
                size: meta.as_ref().map_or(0, std::fs::Metadata::len),
                modified: meta
                    .and_then(|m| m.modified().ok())
                    .map(DateTime::<Utc>::from),
                filename: c.filename,
                description: c.description,
            }
        })
        .collect();
    Ok(json(200, serde_json::json!({ "docs": listed })))
}

/// `GET /v1/projects/{project}/docs/{filename}`: a listed doc as Markdown,
/// empty when its file doesn't exist yet.
pub(super) fn get(root: &Path, filename: &str) -> Result<Reply> {
    if !categories(root)?.iter().any(|c| c.filename == filename) {
        return Ok(error(404, "no such doc"));
    }
    let body = match std::fs::read_to_string(paths::docs_dir(root).join(filename)) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    Ok(Reply::Body {
        status: 200,
        content_type: MARKDOWN,
        body,
        etag: None,
    })
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::super::Config;
    use super::super::routes::{Reply, route};
    use super::super::tests::{pair, request};
    use crate::state::paths;
    use crate::state::serve_files::ServeFiles;
    use crate::testing::TestServer;

    #[test]
    fn only_listed_docs_are_served() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, projects_dir, name) = server.setup_project_no_tmux(dir.path());
        let config = Config::new(
            projects_dir,
            ServeFiles::new(dir.path().into(), dir.path().into()),
            None,
        );
        let bearer = format!("Bearer {}", pair(&config, "phone"));
        let docs = paths::docs_dir(&project);
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(
            docs.join("categories.toml"),
            r#"
[[category]]
filename = "todo.md"
description = "Tasks."

[[category]]
filename = "later.md"
description = "Not written yet."

[[category]]
filename = "../notes.md"
description = "Outside the docs dir."
"#,
        )
        .unwrap();
        std::fs::write(docs.join("todo.md"), "# Todo\n").unwrap();
        std::fs::write(docs.join("secret.md"), "unlisted\n").unwrap();
        std::fs::write(paths::notes_path(&project), "notes\n").unwrap();
        let send = |method: &str, path: &str| match route(
            &config,
            "",
            &request(method, path, "", Some(&bearer), ""),
        )
        .reply
        {
            Reply::Body { status, body, .. } => (status, body),
            Reply::Events { .. } => unreachable!(),
        };
        let base = format!("/v1/projects/{name}/docs");

        let (status, body) = send("GET", &base);
        assert_eq!(status, 200);
        let listed: serde_json::Value = serde_json::from_str(&body).unwrap();
        let docs = listed["docs"].as_array().unwrap();
        let names: Vec<_> = docs
            .iter()
            .map(|d| d["filename"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["todo.md", "later.md"]);
        assert_eq!(docs[0]["description"], "Tasks.");
        assert_eq!(docs[0]["size"], 7);
        assert!(docs[0]["modified"].is_string());
        assert_eq!(docs[1]["size"], 0);
        assert!(docs[1]["modified"].is_null());

        assert_eq!(
            send("GET", &format!("{base}/todo.md")),
            (200, "# Todo\n".into())
        );
        assert_eq!(
            send("GET", &format!("{base}/later.md")),
            (200, String::new())
        );
        for unlisted in ["secret.md", "categories.toml", "..%2Fnotes.md", "%2E%2E"] {
            assert_eq!(
                send("GET", &format!("{base}/{unlisted}")).0,
                404,
                "{unlisted}"
            );
        }
        assert_eq!(send("GET", &format!("{base}/todo.md/x")).0, 404);
        assert_eq!(send("PUT", &format!("{base}/todo.md")).0, 405);
        assert_eq!(send("GET", "/v1/projects/nope/docs").0, 404);
    }
}
