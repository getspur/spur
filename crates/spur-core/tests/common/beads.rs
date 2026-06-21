use std::path::Path;
use std::sync::Arc;

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use spur_pm::test_workspace::TestBeadsWorkspace;

pub fn attach_beads_workspace(repo: &Path, w: &TestBeadsWorkspace) {
    let beads_dir = repo.join(".beads");
    std::fs::create_dir_all(&beads_dir).expect("create test .beads directory");
    w.copy_db_to(&beads_dir);
}

pub fn init_beads_repo(repo: &Path) -> TestBeadsWorkspace {
    let w = TestBeadsWorkspace::init();
    attach_beads_workspace(repo, &w);
    w
}

pub async fn init_beads_pm(repo: &Path) -> (TestBeadsWorkspace, Arc<spur_pm::PmService>) {
    let w = init_beads_repo(repo);
    let pm = Arc::new(
        spur_pm::PmService::try_new(None, true, false, repo, None)
            .await
            .expect("PmService::try_new failed")
            .expect("expected beads pm"),
    );
    (w, pm)
}

pub fn run_br(repo: &Path, args: &[&str]) -> Result<String, String> {
    match args.first().copied() {
        Some("init") => {
            init_beads_repo(repo);
            Ok(String::new())
        }
        Some("create") => create_issue(repo, args),
        Some("label") => add_label(repo, args),
        Some("dep") => add_dependency(repo, args),
        Some("update") => update_issue(repo, args),
        Some("close") => close_issues(repo, args),
        Some("comments") => comments(repo, args),
        Some("show") => show_issue(repo, args),
        Some("list") => list_issues(repo),
        Some("sync") => sync_import_only(repo, args),
        other => Err(format!(
            "unsupported test beads command: {other:?} {args:?}"
        )),
    }
}

fn open(repo: &Path) -> Result<Connection, String> {
    Connection::open(repo.join(".beads/beads.db")).map_err(|err| err.to_string())
}

fn now() -> String {
    Utc::now().to_rfc3339()
}

fn create_issue(repo: &Path, args: &[&str]) -> Result<String, String> {
    let mut title: Option<String> = None;
    let mut issue_type = "task".to_string();
    let mut priority = 2_i64;
    let mut labels = Vec::new();
    let mut silent = false;

    let mut i = 1;
    while i < args.len() {
        match args[i] {
            "--json" => {}
            "--silent" => silent = true,
            "-t" | "--type" => {
                i += 1;
                issue_type = args
                    .get(i)
                    .ok_or_else(|| "missing issue type".to_string())?
                    .to_string();
            }
            "--title" => {
                i += 1;
                title = Some(
                    args.get(i)
                        .ok_or_else(|| "missing issue title".to_string())?
                        .to_string(),
                );
            }
            "-p" | "--priority" => {
                i += 1;
                priority = args
                    .get(i)
                    .ok_or_else(|| "missing priority".to_string())?
                    .parse::<i64>()
                    .map_err(|err| format!("invalid priority: {err}"))?;
            }
            "-l" | "--label" => {
                i += 1;
                let label = args
                    .get(i)
                    .ok_or_else(|| "missing label".to_string())?
                    .to_string();
                validate_create_label(&label)?;
                labels.push(label);
            }
            value if !value.starts_with('-') && title.is_none() => {
                title = Some(value.to_string());
            }
            _ => {}
        }
        i += 1;
    }

    let title = title.unwrap_or_else(|| "test issue".to_string());
    let uuid = uuid::Uuid::new_v4().simple().to_string();
    let id = format!("bd-{}", &uuid[..8]);
    let now = now();
    let conn = open(repo)?;
    conn.execute(
        "INSERT INTO issues (
             id, title, description, status, priority, issue_type, created_at,
             updated_at, created_by, source_repo
         ) VALUES (?1, ?2, '', 'open', ?3, ?4, ?5, ?5, 'test', '.')",
        params![id, title, priority, issue_type, now],
    )
    .map_err(|err| err.to_string())?;

    for label in labels {
        conn.execute(
            "INSERT OR IGNORE INTO labels(issue_id, label) VALUES (?1, ?2)",
            params![id, label],
        )
        .map_err(|err| err.to_string())?;
    }

    if silent {
        Ok(format!("\"{id}\""))
    } else {
        Ok(json!({ "id": id }).to_string())
    }
}

fn add_label(repo: &Path, args: &[&str]) -> Result<String, String> {
    if args.len() < 4 || args.get(1) != Some(&"add") {
        return Err(format!("unsupported label command: {args:?}"));
    }
    let issue_id = args[2];
    let label = if args.get(3) == Some(&"-l") || args.get(3) == Some(&"--label") {
        *args.get(4).ok_or_else(|| "missing label".to_string())?
    } else {
        args[3]
    };
    let conn = open(repo)?;
    conn.execute(
        "INSERT OR IGNORE INTO labels(issue_id, label) VALUES (?1, ?2)",
        params![issue_id, label],
    )
    .map_err(|err| err.to_string())?;
    Ok(String::new())
}

fn add_dependency(repo: &Path, args: &[&str]) -> Result<String, String> {
    if args.len() < 4 || args.get(1) != Some(&"add") {
        return Err(format!("unsupported dep command: {args:?}"));
    }
    let conn = open(repo)?;
    let depends_on_type: Option<String> = conn
        .query_row(
            "SELECT issue_type FROM issues WHERE id = ?1",
            params![args[3]],
            |row| row.get(0),
        )
        .optional()
        .map_err(|err| err.to_string())?;
    let dep_type = if depends_on_type.as_deref() == Some("epic") {
        "parent-child"
    } else {
        "blocks"
    };
    conn.execute(
        "INSERT OR IGNORE INTO dependencies(
             issue_id, depends_on_id, type, created_at, created_by, metadata, thread_id
         ) VALUES (?1, ?2, ?3, ?4, 'test', '{}', '')",
        params![args[2], args[3], dep_type, now()],
    )
    .map_err(|err| err.to_string())?;
    rebuild_blocked_cache(&conn)?;
    Ok(String::new())
}

fn update_issue(repo: &Path, args: &[&str]) -> Result<String, String> {
    if args.len() >= 4 && args[2] == "--status" && args[3] == "closed" {
        close_issue(repo, args[1])?;
        Ok(String::new())
    } else {
        Err(format!("unsupported update command: {args:?}"))
    }
}

fn close_issues(repo: &Path, args: &[&str]) -> Result<String, String> {
    for issue_id in &args[1..] {
        if issue_id.starts_with('-') {
            continue;
        }
        close_issue(repo, issue_id)?;
    }
    Ok(String::new())
}

fn close_issue(repo: &Path, issue_id: &str) -> Result<(), String> {
    let conn = open(repo)?;
    let changed = conn
        .execute(
            "UPDATE issues
             SET status = 'closed', closed_at = ?1, updated_at = ?1
             WHERE id = ?2",
            params![now(), issue_id],
        )
        .map_err(|err| err.to_string())?;
    if changed == 0 {
        return Err(format!("issue not found: {issue_id}"));
    }
    rebuild_blocked_cache(&conn)?;
    Ok(())
}

fn comments(repo: &Path, args: &[&str]) -> Result<String, String> {
    match args.get(1).copied() {
        Some("add") => {
            let issue_id = args.get(2).ok_or_else(|| "missing issue id".to_string())?;
            let body = args
                .get(3)
                .ok_or_else(|| "missing comment body".to_string())?;
            let conn = open(repo)?;
            conn.execute(
                "INSERT INTO comments(issue_id, author, text, created_at)
                 VALUES (?1, 'test', ?2, ?3)",
                params![issue_id, body, now()],
            )
            .map_err(|err| err.to_string())?;
            let id = conn.last_insert_rowid();
            Ok(json!({ "id": id }).to_string())
        }
        Some("list") => {
            let issue_id = args.get(2).ok_or_else(|| "missing issue id".to_string())?;
            let conn = open(repo)?;
            let mut stmt = conn
                .prepare(
                    "SELECT id, author, text, created_at
                     FROM comments
                     WHERE issue_id = ?1
                     ORDER BY created_at ASC, id ASC",
                )
                .map_err(|err| err.to_string())?;
            let rows = stmt
                .query_map(params![issue_id], |row| {
                    Ok(json!({
                        "id": row.get::<_, i64>(0)?,
                        "author": row.get::<_, String>(1)?,
                        "text": row.get::<_, String>(2)?,
                        "body": row.get::<_, String>(2)?,
                        "created_at": row.get::<_, String>(3)?,
                    }))
                })
                .map_err(|err| err.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|err| err.to_string())?;
            Ok(Value::Array(rows).to_string())
        }
        _ => Err(format!("unsupported comments command: {args:?}")),
    }
}

fn show_issue(repo: &Path, args: &[&str]) -> Result<String, String> {
    let issue_id = args.get(1).ok_or_else(|| "missing issue id".to_string())?;
    let conn = open(repo)?;
    let mut issue =
        issue_json(&conn, issue_id)?.ok_or_else(|| format!("issue not found: {issue_id}"))?;
    issue["labels"] = Value::Array(
        labels_for(&conn, issue_id)?
            .into_iter()
            .map(Value::String)
            .collect(),
    );
    issue["dependencies"] = Value::Array(
        dependencies_for(&conn, issue_id)?
            .into_iter()
            .map(|id| json!({ "id": id, "type": "blocks" }))
            .collect(),
    );
    Ok(Value::Array(vec![issue]).to_string())
}

fn list_issues(repo: &Path) -> Result<String, String> {
    let conn = open(repo)?;
    let mut stmt = conn
        .prepare(
            "SELECT id FROM issues
             WHERE deleted_at IS NULL
             ORDER BY created_at DESC, id ASC",
        )
        .map_err(|err| err.to_string())?;
    let ids = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|err| err.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| err.to_string())?;

    let mut issues = Vec::new();
    for id in ids {
        if let Some(mut issue) = issue_json(&conn, &id)? {
            issue["labels"] = Value::Array(
                labels_for(&conn, &id)?
                    .into_iter()
                    .map(Value::String)
                    .collect(),
            );
            issues.push(issue);
        }
    }
    Ok(json!({ "issues": issues }).to_string())
}

fn sync_import_only(repo: &Path, args: &[&str]) -> Result<String, String> {
    if args != ["sync", "--import-only"] {
        return Err(format!("unsupported sync command: {args:?}"));
    }

    let jsonl_path = repo.join(".beads/issues.jsonl");
    let contents = std::fs::read_to_string(&jsonl_path).map_err(|err| err.to_string())?;
    let conn = open(repo)?;
    for (idx, line) in contents.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let issue: Value = serde_json::from_str(line)
            .map_err(|err| format!("invalid issues.jsonl line {}: {err}", idx + 1))?;
        let id = issue
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("issues.jsonl line {} missing id", idx + 1))?;
        let title = issue.get("title").and_then(Value::as_str).unwrap_or("");
        let description = issue
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("");
        let status = issue
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("open");
        let priority = issue.get("priority").and_then(Value::as_i64).unwrap_or(2);
        let issue_type = issue
            .get("issue_type")
            .and_then(Value::as_str)
            .unwrap_or("task");
        let created_at = issue
            .get("created_at")
            .and_then(Value::as_str)
            .unwrap_or(line);
        let updated_at = issue
            .get("updated_at")
            .and_then(Value::as_str)
            .unwrap_or(created_at);
        let created_by = issue
            .get("created_by")
            .and_then(Value::as_str)
            .unwrap_or("test");
        let source_repo = issue
            .get("source_repo")
            .and_then(Value::as_str)
            .unwrap_or(".");

        conn.execute(
            "INSERT OR REPLACE INTO issues (
                 id, title, description, status, priority, issue_type, created_at,
                 updated_at, created_by, source_repo
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                id,
                title,
                description,
                status,
                priority,
                issue_type,
                created_at,
                updated_at,
                created_by,
                source_repo
            ],
        )
        .map_err(|err| err.to_string())?;
    }
    Ok(String::new())
}

fn validate_create_label(label: &str) -> Result<(), String> {
    if label.is_empty() {
        return Err("Validation failed: label: cannot be empty".to_string());
    }
    if label.len() > 50 {
        return Err("Validation failed: label: exceeds 50 characters".to_string());
    }
    if !label
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == ':')
    {
        return Err(
            "Validation failed: label: invalid characters (only alphanumeric, hyphen, underscore, colon allowed)"
                .to_string(),
        );
    }
    Ok(())
}

fn rebuild_blocked_cache(conn: &Connection) -> Result<(), String> {
    conn.execute("DELETE FROM blocked_issues_cache", [])
        .map_err(|err| err.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT d.issue_id, d.depends_on_id
             FROM dependencies d
             JOIN issues blocker ON blocker.id = d.depends_on_id
             WHERE d.type IN ('blocks', 'conditional-blocks', 'waits-for')
               AND blocker.status != 'closed'
             ORDER BY d.issue_id, d.depends_on_id",
        )
        .map_err(|err| err.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|err| err.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| err.to_string())?;

    let mut by_issue = std::collections::BTreeMap::<String, Vec<String>>::new();
    for (issue_id, blocked_by) in rows {
        by_issue.entry(issue_id).or_default().push(blocked_by);
    }

    for (issue_id, blocked_by) in by_issue {
        let blocked_by_json = serde_json::to_string(&blocked_by).map_err(|err| err.to_string())?;
        conn.execute(
            "INSERT INTO blocked_issues_cache(issue_id, blocked_by, blocked_at)
             VALUES (?1, ?2, CURRENT_TIMESTAMP)",
            params![issue_id, blocked_by_json],
        )
        .map_err(|err| err.to_string())?;
    }
    Ok(())
}

fn issue_json(conn: &Connection, issue_id: &str) -> Result<Option<Value>, String> {
    conn.query_row(
        "SELECT id, title, description, status, priority, issue_type, created_at,
                updated_at, created_by
         FROM issues
         WHERE id = ?1",
        params![issue_id],
        |row| {
            Ok(json!({
                "id": row.get::<_, String>(0)?,
                "title": row.get::<_, String>(1)?,
                "description": row.get::<_, String>(2)?,
                "status": row.get::<_, String>(3)?,
                "priority": row.get::<_, i64>(4)?,
                "issue_type": row.get::<_, String>(5)?,
                "created_at": row.get::<_, String>(6)?,
                "updated_at": row.get::<_, String>(7)?,
                "created_by": row.get::<_, Option<String>>(8)?,
            }))
        },
    )
    .optional()
    .map_err(|err| err.to_string())
}

fn labels_for(conn: &Connection, issue_id: &str) -> Result<Vec<String>, String> {
    let mut stmt = conn
        .prepare("SELECT label FROM labels WHERE issue_id = ?1 ORDER BY label")
        .map_err(|err| err.to_string())?;
    let labels = stmt
        .query_map(params![issue_id], |row| row.get::<_, String>(0))
        .map_err(|err| err.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| err.to_string())?;
    Ok(labels)
}

fn dependencies_for(conn: &Connection, issue_id: &str) -> Result<Vec<String>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT depends_on_id FROM dependencies
             WHERE issue_id = ?1
             ORDER BY depends_on_id",
        )
        .map_err(|err| err.to_string())?;
    let dependencies = stmt
        .query_map(params![issue_id], |row| row.get::<_, String>(0))
        .map_err(|err| err.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| err.to_string())?;
    Ok(dependencies)
}
