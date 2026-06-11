use std::path::{Path, PathBuf};

use agent_client_protocol::schema::{EnvVariable, McpServer, McpServerStdio};
use anyhow::{anyhow, Context, Result};

use super::types::AppScope;
use crate::spur_app::{SpurAppManifest, SpurAppMcpServer, SPUR_APP_MANIFEST, SPUR_APP_SCHEMA};

const DEFAULT_NOTEBOOK_APP_KEY: &str = "notebook";
const DEFAULT_NOTEBOOK_LABEL: &str = "Notebook";
const DEFAULT_SKILL_PATH: &str = "skill/SKILL.md";

pub fn resolve_app_scope(notebook_path: &Path) -> Result<AppScope> {
    let notebook_dir = notebook_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    let Some((app_root, manifest_path)) = find_manifest_dir(&notebook_dir) else {
        return Ok(default_notebook_scope(notebook_dir));
    };

    let manifest_bytes = std::fs::read(&manifest_path)
        .with_context(|| format!("failed to read app manifest {}", manifest_path.display()))?;
    let manifest: SpurAppManifest = serde_json::from_slice(&manifest_bytes)
        .with_context(|| format!("failed to parse app manifest {}", manifest_path.display()))?;

    if manifest.schema != SPUR_APP_SCHEMA {
        return Err(anyhow!(
            "unsupported Spur App schema {:?} in {}",
            manifest.schema,
            manifest_path.display()
        ));
    }

    let mut mcp_servers = foundation_mcp_servers();
    if let Some(mcp_server) = &manifest.mcp_server {
        mcp_servers.push(app_mcp_server(&manifest.name, mcp_server)?);
    }

    Ok(AppScope {
        cwd: app_root.clone(),
        mcp_servers,
        skill: read_skill(&app_root, manifest.skill.as_deref())?,
        app_key: app_root.display().to_string(),
        label: manifest.name,
    })
}

fn default_notebook_scope(cwd: PathBuf) -> AppScope {
    AppScope {
        cwd,
        mcp_servers: foundation_mcp_servers(),
        skill: None,
        app_key: DEFAULT_NOTEBOOK_APP_KEY.to_string(),
        label: DEFAULT_NOTEBOOK_LABEL.to_string(),
    }
}

fn foundation_mcp_servers() -> Vec<McpServer> {
    Vec::new()
}

fn find_manifest_dir(start: &Path) -> Option<(PathBuf, PathBuf)> {
    for candidate in start.ancestors() {
        let manifest_path = candidate.join(SPUR_APP_MANIFEST);
        if manifest_path.is_file() {
            return Some((candidate.to_path_buf(), manifest_path));
        }
    }
    None
}

fn read_skill(app_root: &Path, skill_path: Option<&str>) -> Result<Option<String>> {
    let explicit_skill = skill_path.map(|path| app_root.join(path));
    if let Some(path) = explicit_skill {
        return std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read app skill {}", path.display()))
            .map(Some);
    }

    let default_path = app_root.join(DEFAULT_SKILL_PATH);
    if default_path.is_file() {
        return std::fs::read_to_string(&default_path)
            .with_context(|| format!("failed to read app skill {}", default_path.display()))
            .map(Some);
    }

    Ok(None)
}

fn app_mcp_server(name: &str, manifest: &SpurAppMcpServer) -> Result<McpServer> {
    if manifest.requirements.is_some() {
        return Err(anyhow!(
            "app MCP server requirements are not representable in ACP McpServer without plugin provisioning"
        ));
    }

    Ok(McpServer::Stdio(
        McpServerStdio::new(name, manifest.server_type.clone())
            .args(vec![manifest.entry.clone()])
            .env(
                manifest
                    .env
                    .iter()
                    .map(|(name, value)| EnvVariable::new(name.clone(), value.clone()))
                    .collect(),
            ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::schema::McpServer;

    #[test]
    fn plain_notebook_yields_default_scope() {
        let dir = tempfile::tempdir().unwrap();
        let nb = dir.path().join("notebook.ipynb");
        std::fs::write(&nb, "{}").unwrap();

        let scope = resolve_app_scope(&nb).unwrap();

        assert_eq!(scope.cwd, dir.path());
        assert_eq!(scope.label, "Notebook");
        assert_eq!(scope.app_key, "notebook");
        assert!(scope.skill.is_none());
        assert!(scope.mcp_servers.is_empty());
    }

    #[test]
    fn spur_app_dir_yields_app_scope_with_skill_and_mcp() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("spur-app.json"),
            r#"{
              "schema": "spur.app/v1",
              "name": "Code Graph Workbench",
              "entry_notebook": "app.ipynb",
              "open_mode": "app",
              "runtime": {
                "jute_min": "0.1.0",
                "features": ["frontend-cells", "anywidget-afm", "ports-arrow"]
              },
              "mcp_server": {
                "type": "python",
                "entry": "server/main.py",
                "env": { "SPUR_APP_MODE": "test" }
              },
              "skill": "skill/SKILL.md"
            }"#,
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("skill")).unwrap();
        std::fs::write(dir.path().join("skill/SKILL.md"), "workbench skill").unwrap();
        let nb = dir.path().join("app.ipynb");
        std::fs::write(&nb, "{}").unwrap();

        let scope = resolve_app_scope(&nb).unwrap();

        assert_eq!(scope.label, "Code Graph Workbench");
        assert_eq!(scope.cwd, dir.path());
        assert_eq!(scope.app_key, dir.path().display().to_string());
        assert_eq!(scope.skill.as_deref(), Some("workbench skill"));
        assert_eq!(scope.mcp_servers.len(), 1);
        match &scope.mcp_servers[0] {
            McpServer::Stdio(server) => {
                assert_eq!(server.name, "Code Graph Workbench");
                assert_eq!(server.command, std::path::PathBuf::from("python"));
                assert_eq!(server.args, vec!["server/main.py"]);
                assert!(server
                    .env
                    .iter()
                    .any(|env| env.name == "SPUR_APP_MODE" && env.value == "test"));
            }
            other => panic!("expected stdio MCP server, got {other:?}"),
        }
    }
}
