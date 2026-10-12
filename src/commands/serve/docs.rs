//! `/v1/projects/{project}/docs[/{filename}]`: the project's information
//! store, read-only, through [`crate::state::docs`].

use std::path::Path;

use crate::error::Result;
use crate::state::docs;

use super::routes::{MARKDOWN, Reply, error, json};

/// `GET /v1/projects/{project}/docs`.
pub(super) fn list(root: &Path) -> Result<Reply> {
    Ok(json(200, serde_json::json!({ "docs": docs::list(root)? })))
}

/// `GET /v1/projects/{project}/docs/{filename}`: a listed doc as Markdown,
/// empty when its file doesn't exist yet.
pub(super) fn get(root: &Path, filename: &str) -> Result<Reply> {
    Ok(match docs::read(root, filename)? {
        Some(body) => Reply::Body {
            status: 200,
            content_type: MARKDOWN,
            body,
            etag: None,
        },
        None => error(404, "no such doc"),
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
filename = "alias.md"
description = "A link that stays in the docs dir."

[[category]]
filename = "sub"
description = "A directory."

[[category]]
filename = "escape.md"
description = "A link out of it."

[[category]]
filename = "../notes.md"
description = "Outside the docs dir."
"#,
        )
        .unwrap();
        std::fs::write(docs.join("todo.md"), "# Todo\n").unwrap();
        std::fs::write(docs.join("secret.md"), "unlisted\n").unwrap();
        std::fs::write(docs.join("ideas.md"), "# Ideas\n").unwrap();
        std::os::unix::fs::symlink("ideas.md", docs.join("alias.md")).unwrap();
        std::fs::create_dir(docs.join("sub")).unwrap();
        std::os::unix::fs::symlink("../notes.md", docs.join("escape.md")).unwrap();
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
        assert_eq!(names, ["todo.md", "later.md", "alias.md"]);
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
        assert_eq!(
            send("GET", &format!("{base}/alias.md")),
            (200, "# Ideas\n".into())
        );
        for unlisted in [
            "sub",
            "escape.md",
            "secret.md",
            "categories.toml",
            "..%2Fnotes.md",
            "%2E%2E",
        ] {
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
