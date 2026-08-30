# MCP Server Gateway Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `docs/superpowers/specs/2026-08-30-mcp-server-gateway-design.ipynb`
**Formal @spec cells (if notebook):** `MCP-ENTRY-VALIDATION`, `MCP-SESSION-INJECTION`
**Design epic:** `bd-3ii8a` (closed)

**Goal:** User-configurable MCP servers (stdio + HTTP) persisted in `SpurConfig`, managed through a new `/configure mcp` TUI section, injected into brain sessions at `NewSessionRequest` time.

**Architecture:** New `mcp_servers` config section + `ConfigPatch` variants in `spur-acp` (validation gate: reserved names, non-empty payloads, upsert-replace semantics). `spur-core` appends enabled entries after the fixed servers in `brain_mcp_servers`. `spur-tui` adds the MCP section to the existing `/configure` SAVE-APPLY flow. Workers/direct-exec remain untouched (curated catalog only).

**Tech Stack:** Rust 2021, serde/TOML, agent-client-protocol 1.2.0 schema types (`McpServerStdio`/`McpServerHttp`/`EnvVariable`/`HttpHeader`).

**Worker routing (user-specified):** all tasks → agent `codex`, profile `rust-engineer`, model `gpt-5.6-sol`, effort `xhigh`.

---

### Task 1: spur-acp config schema, ConfigPatch, validation gate

**Task ID:** `task-1`

**Files:**
- Modify: `crates/spur-acp/src/config/mod.rs` (SpurConfig struct ~line 560-586; `ConfigPatch` ~line 631-693; reserved-name const near new types)
- Test: inline `mod tests` in the same file (follow existing config test conventions)

**Depends on:** none

**Acceptance Criteria:**
- [ ] `SpurConfig.mcp_servers` field exists with `#[serde(default, skip_serializing_if = "McpServersConfig::is_default")]`
- [ ] `ConfigPatch::McpServerUpsert` / `ConfigPatch::McpServerRemove` with `section_id() == "mcp"`
- [ ] Validation rejects: empty name, reserved name (`spur-mcp`, `notebook`, `spur-worker-mcp`), empty stdio command, empty/non-http(s) url
- [ ] Upsert with existing name replaces in place (no duplicates possible); Remove of missing name errors
- [ ] TOML round-trip test passes; both-transport TOML fails to deserialize
- [ ] `scripts/spur-cargo test -p spur-acp` green; `scripts/spur-cargo clippy -p spur-acp -- -D warnings` green

**Suggested Worker:** codex / rust-engineer / gpt-5.6-sol / xhigh

**Scope Boundary:**
- IN scope: `crates/spur-acp/src/config/mod.rs` only
- OUT of scope: spur-core, spur-tui, any SDK type changes
- If you need to touch OUT-OF-SCOPE files → emit `scope_drift` signal immediately

**Implementation:**

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod mcp_servers_config_tests {
    use super::*;

    fn http_entry(name: &str) -> McpServerEntry {
        McpServerEntry {
            name: name.to_owned(),
            enabled: true,
            transport: McpServerTransport::Http {
                url: "https://mcp.example.com/sse".to_owned(),
                headers: std::collections::HashMap::new(),
            },
        }
    }

    fn stdio_entry(name: &str) -> McpServerEntry {
        McpServerEntry {
            name: name.to_owned(),
            enabled: true,
            transport: McpServerTransport::Stdio {
                command: "npx".to_owned(),
                args: vec!["-y".to_owned(), "@modelcontextprotocol/server-github".to_owned()],
                env: std::collections::HashMap::new(),
            },
        }
    }

    #[test]
    fn toml_round_trip_preserves_entries() {
        let src = r#"
[[mcp_servers.entries]]
name = "github"
enabled = false

[mcp_servers.entries.transport]
transport = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]

[mcp_servers.entries.transport.env]
GITHUB_TOKEN = "tok"
"#;
        // NOTE: adjust the TOML shape to the serde tagging you implement;
        // the assertion is the round trip, not this exact literal.
        let cfg: SpurConfig = toml::from_str(src).expect("parse");
        assert_eq!(cfg.mcp_servers.entries.len(), 1);
        let entry = &cfg.mcp_servers.entries[0];
        assert_eq!(entry.name, "github");
        assert!(!entry.enabled);
        let out = toml::to_string(&cfg).expect("serialize");
        let back: SpurConfig = toml::from_str(&out).expect("re-parse");
        assert_eq!(back.mcp_servers, cfg.mcp_servers);
    }

    #[test]
    fn both_transport_blocks_is_deserialize_error() {
        let src = r#"
[[mcp_servers.entries]]
name = "bad"

[mcp_servers.entries.transport]
transport = "stdio"
command = "npx"
url = "https://x"
"#;
        assert!(toml::from_str::<SpurConfig>(src).is_err());
    }

    #[test]
    fn validate_rejects_reserved_and_empty() {
        let mut e = http_entry("spur-mcp");
        assert!(e.validate().is_err(), "reserved name");
        e = http_entry("notebook");
        assert!(e.validate().is_err());
        e = http_entry("spur-worker-mcp");
        assert!(e.validate().is_err());
        e.name = "  ".to_owned();
        assert!(e.validate().is_err(), "empty name");
    }

    #[test]
    fn validate_rejects_empty_payloads() {
        let mut e = stdio_entry("s1");
        if let McpServerTransport::Stdio { command, .. } = &mut e.transport {
            *command = "   ".to_owned();
        }
        assert!(e.validate().is_err(), "empty command");

        let mut h = http_entry("h1");
        if let McpServerTransport::Http { url, .. } = &mut h.transport {
            *url = "ftp://nope".to_owned();
        }
        assert!(h.validate().is_err(), "non-http scheme");
        if let McpServerTransport::Http { url, .. } = &mut h.transport {
            *url = String::new();
        }
        assert!(h.validate().is_err(), "empty url");
    }

    #[test]
    fn upsert_replaces_in_place_and_remove_missing_errors() {
        let mut cfg = SpurConfig::default();
        ConfigPatch::McpServerUpsert { entry: http_entry("a") }.apply(&mut cfg).unwrap();
        ConfigPatch::McpServerUpsert { entry: stdio_entry("b") }.apply(&mut cfg).unwrap();
        let replaced = http_entry("a2");
        ConfigPatch::McpServerUpsert { entry: replaced }.apply(&mut cfg).unwrap();
        assert_eq!(cfg.mcp_servers.entries.len(), 2, "upsert replaces, never duplicates");
        assert_eq!(cfg.mcp_servers.entries[0].name, "a2");

        let err = ConfigPatch::McpServerRemove { name: "zzz".into() }.apply(&mut cfg);
        assert!(err.is_err(), "remove missing name must fail");
        ConfigPatch::McpServerRemove { name: "b".into() }.apply(&mut cfg).unwrap();
        assert_eq!(cfg.mcp_servers.entries.len(), 1);
    }

    #[test]
    fn patch_section_id_is_mcp() {
        assert_eq!(ConfigPatch::McpServerUpsert { entry: http_entry("x") }.section_id(), "mcp");
        assert_eq!(ConfigPatch::McpServerRemove { name: "x".into() }.section_id(), "mcp");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `scripts/spur-cargo test -p spur-acp mcp_servers_config`
Expected: FAIL — `McpServersConfig`/`McpServerEntry`/`McpServerTransport` not defined

- [ ] **Step 3: Write the implementation**

```rust
/// Names users may not assign to configured MCP servers. Colliding with a
/// SPUR-managed entry would shadow the fixed injection set.
pub const RESERVED_MCP_SERVER_NAMES: &[&str] =
    &["spur-mcp", "notebook", "spur-worker-mcp"];

fn default_true() -> bool {
    true
}

/// User-configured MCP servers (`/configure mcp`). Injected into brain
/// sessions only; workers keep the curated `spur-worker-mcp` catalog.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct McpServersConfig {
    pub entries: Vec<McpServerEntry>,
}

impl McpServersConfig {
    pub fn is_default(&self) -> bool {
        self.entries.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerEntry {
    /// Unique, non-reserved identifier (`RESERVED_MCP_SERVER_NAMES`).
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub transport: McpServerTransport,
}

/// Exactly one transport per entry — enum makes "both" unrepresentable in
/// Rust; serde rejects a second `transport` key at the TOML level.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "transport", rename_all = "lowercase")]
pub enum McpServerTransport {
    Stdio {
        command: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        args: Vec<String>,
        #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
        env: std::collections::HashMap<String, String>,
    },
    Http {
        url: String,
        #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
        headers: std::collections::HashMap<String, String>,
    },
}

impl McpServerEntry {
    /// MCP-ENTRY-VALIDATION gate (payload checks; uniqueness is enforced by
    /// upsert-replace in `ConfigPatch::apply`).
    pub fn validate(&self) -> anyhow::Result<()> {
        let name = self.name.trim();
        anyhow::ensure!(!name.is_empty(), "mcp server name must not be empty");
        anyhow::ensure!(
            !RESERVED_MCP_SERVER_NAMES.contains(&name),
            "mcp server name '{name}' is reserved"
        );
        match &self.transport {
            McpServerTransport::Stdio { command, .. } => anyhow::ensure!(
                !command.trim().is_empty(),
                "mcp server '{}' stdio command must not be empty",
                self.name
            ),
            McpServerTransport::Http { url, .. } => {
                let url = url.trim();
                anyhow::ensure!(
                    !url.is_empty(),
                    "mcp server '{}' http url must not be empty",
                    self.name
                );
                anyhow::ensure!(
                    url.starts_with("http://") || url.starts_with("https://"),
                    "mcp server '{}' http url must use http(s)",
                    self.name
                );
            }
        }
        Ok(())
    }
}
```

Add to `SpurConfig` (after `tui`, before `graph`):

```rust
    /// User-configured MCP servers injected into brain sessions.
    #[serde(default, skip_serializing_if = "McpServersConfig::is_default")]
    pub mcp_servers: McpServersConfig,
```

Extend `ConfigPatch`:

```rust
    McpServerUpsert {
        entry: McpServerEntry,
    },
    McpServerRemove {
        name: String,
    },
```

`section_id()` arm: `Self::McpServerUpsert { .. } | Self::McpServerRemove { .. } => "mcp",`

`apply()` arms:

```rust
            Self::McpServerUpsert { entry } => {
                entry.validate()?;
                match cfg
                    .mcp_servers
                    .entries
                    .iter_mut()
                    .find(|e| e.name == entry.name)
                {
                    Some(slot) => *slot = entry,
                    None => cfg.mcp_servers.entries.push(entry),
                }
            }
            Self::McpServerRemove { name } => {
                let before = cfg.mcp_servers.entries.len();
                cfg.mcp_servers.entries.retain(|e| e.name != *name);
                anyhow::ensure!(
                    cfg.mcp_servers.entries.len() < before,
                    "mcp server '{name}' is not configured"
                );
            }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `scripts/spur-cargo test -p spur-acp`
Expected: PASS (new module + full crate)
Run: `scripts/spur-cargo clippy -p spur-acp -- -D warnings`
Expected: clean

- [ ] **Step 5: Commit**

```bash
git add crates/spur-acp/src/config/mod.rs
git commit -m "feat(spur-acp): task-1 mcp_servers config schema, ConfigPatch, validation gate"
```

**Scope Drift Checkpoint:**
- If serde tagging choice requires touching seed agents or other config files → emit `scope_drift`
- If `SpurConfig` derive bounds break other crates at compile time, fix within `config/mod.rs` (e.g., `Default` impls) — anything beyond → emit `scope_drift`

---

### Task 2: spur-core brain session injection

**Task ID:** `task-2`

**Files:**
- Modify: `crates/spur-core/src/notebook.rs:402-425` (`brain_mcp_servers`)
- Modify: `crates/spur-core/src/orchestrator/session.rs:2212`, `session.rs:2536`, `crates/spur-core/src/orchestrator/adhoc.rs:116` (call sites — pass `&self.config.mcp_servers`)
- Test: `crates/spur-core/tests/notebook_mcp_config.rs` (extend)

**Depends on:** task-1

**Acceptance Criteria:**
- [ ] `brain_mcp_servers(spur_mcp_url, socket_nonce, user: &McpServersConfig)` returns fixed servers first (`spur-mcp`, notebook proxy), then enabled user entries in config order
- [ ] Disabled entries excluded; empty config → identical to previous behavior (2 entries, or 1 with `disable-notebook-mcp`)
- [ ] All three call sites pass `&self.config.mcp_servers`
- [ ] Worker/direct paths untouched (`worker_mcp.rs` unchanged)
- [ ] `scripts/spur-cargo test -p spur-core` green; clippy clean

**Suggested Worker:** codex / rust-engineer / gpt-5.6-sol / xhigh

**Scope Boundary:**
- IN scope: the four files above
- OUT of scope: `worker_mcp.rs`, `worker_server.rs`, ACP envelope changes
- If You need to touch OUT-OF-SCOPE files → emit `scope_drift` signal immediately

**Implementation:**

- [ ] **Step 1: Write the failing tests** (append to `crates/spur-core/tests/notebook_mcp_config.rs`)

```rust
fn user_cfg(entries: &[spur_acp::config::McpServerEntry]) -> spur_acp::config::McpServersConfig {
    spur_acp::config::McpServersConfig { entries: entries.to_vec() }
}

#[test]
fn brain_mcp_servers_appends_enabled_user_entries_after_fixed() {
    use spur_acp::config::{McpServerEntry, McpServerTransport};
    let mut gh = McpServerEntry {
        name: "github".to_owned(),
        enabled: true,
        transport: McpServerTransport::Stdio {
            command: "npx".to_owned(),
            args: vec!["-y".into(), "@modelcontextprotocol/server-github".into()],
            env: [("GITHUB_TOKEN".to_owned(), "tok".to_owned())].into_iter().collect(),
        },
    };
    let remote = McpServerEntry {
        name: "remote".to_owned(),
        enabled: false, // disabled → excluded
        transport: McpServerTransport::Http {
            url: "https://mcp.example.com/sse".to_owned(),
            headers: [("Authorization".to_owned(), "Bearer x".to_owned())].into_iter().collect(),
        },
    };
    let servers =
        spur_core::notebook::brain_mcp_servers("http://127.0.0.1:3939/mcp", "fixture-nonce", &user_cfg(&[gh.clone(), remote]))
            .expect("assemble");
    // fixed first
    assert_eq!(servers[0].name(), "spur-mcp");
    assert_eq!(servers[1].name(), "notebook");
    // then enabled user entries in order
    assert_eq!(servers.len(), 3);
    assert_eq!(servers[2].name(), "github");
    match &servers[2] {
        spur_acp::McpServer::Stdio(s) => {
            assert_eq!(s.command, std::path::PathBuf::from("npx"));
            assert_eq!(s.args, vec!["-y".to_owned(), "@modelcontextprotocol/server-github".to_owned()]);
            assert_eq!(s.env.len(), 1);
            assert_eq!(s.env[0].name, "GITHUB_TOKEN");
        }
        other => panic!("expected stdio, got {other:?}"),
    }
    // empty config keeps the historical shape
    let baseline = spur_core::notebook::brain_mcp_servers("http://127.0.0.1:3939/mcp", "fixture-nonce", &user_cfg(&[])).unwrap();
    assert_eq!(baseline.len(), 2);
    gh.enabled = false;
    let all_disabled = spur_core::notebook::brain_mcp_servers("http://127.0.0.1:3939/mcp", "n", &user_cfg(&[gh])).unwrap();
    assert_eq!(all_disabled.len(), 2);
}
```

(If `McpServer` has no `.name()` helper, match on variants to read `.name` — adjust the asserts, keep the ordering/enabled assertions.)

- [ ] **Step 2: Run to verify failure**

Run: `scripts/spur-cargo test -p spur-core notebook_mcp_config`
Expected: FAIL — `brain_mcp_servers` takes 2 args

- [ ] **Step 3: Implement**

In `crates/spur-core/src/notebook.rs`:

```rust
use spur_acp::config::{McpServerEntry, McpServerTransport, McpServersConfig};

/// Convert one configured entry to the ACP SDK shape.
fn user_mcp_server(entry: &McpServerEntry) -> McpServer {
    match &entry.transport {
        McpServerTransport::Stdio { command, args, env } => McpServer::Stdio(
            McpServerStdio::new(entry.name.clone(), command.clone())
                .args(args.clone())
                .env(
                    env.iter()
                        .map(|(k, v)| EnvVariable::new(k.clone(), v.clone()))
                        .collect(),
                ),
        ),
        McpServerTransport::Http { url, headers } => McpServer::Http(
            McpServerHttp::new(entry.name.clone(), url.clone()).headers(
                headers
                    .iter()
                    .map(|(k, v)| HttpHeader::new(k.clone(), v.clone()))
                    .collect(),
            ),
        ),
    }
}

pub fn brain_mcp_servers(
    spur_mcp_url: &str,
    socket_nonce: &str,
    user: &McpServersConfig,
) -> Result<Vec<McpServer>, NotebookResolverError> {
    let mut servers = existing_body(spur_mcp_url, socket_nonce)?; // the current 2-entry logic
    servers.extend(user.entries.iter().filter(|e| e.enabled).map(user_mcp_server));
    Ok(servers)
}
```

(`EnvVariable::new` / `HttpHeader::new` are the SDK builders; if `EnvVariable::new` does not exist use a struct literal with `meta: None`. `HttpHeader::new` is already used in `worker_mcp.rs:161`.)

Import `EnvVariable` from `spur_acp` (add to the existing re-export list in `crates/spur-acp/src/lib.rs:134-149` **only if missing** — one-line addition, allowed exception to scope).

Update the three call sites to `crate::notebook::brain_mcp_servers(&mcp_url, &socket_nonce, &self.config.mcp_servers)?`.

- [ ] **Step 4: Run to verify pass** — `scripts/spur-cargo test -p spur-core && scripts/spur-cargo clippy -p spur-core -- -D warnings`

- [ ] **Step 5: Commit** — `feat(spur-core): task-2 inject user mcp servers into brain sessions`

**Scope Drift Checkpoint:**
- If `Orchestrator.config` is not reachable at a call site (borrow/ownership), stop and emit `scope_drift` with the borrow error
- Do NOT touch `build_worker_mcp_servers_with` / `build_direct_mcp_servers_with` — that is the curated worker catalog

---

### Task 3: spur-tui `/configure mcp` section

**Task ID:** `task-3`

**Files:**
- Modify: `crates/spur-tui/src/configure_section.rs` (add `Mcp` variant: `ALL` → 5, `parse_token` `"mcp"`, `as_str` `"mcp"`, `list_label` `"MCP Servers"`)
- Create: `crates/spur-tui/src/views/mcp_servers_tui.rs` (section pane)
- Modify: `crates/spur-tui/src/views/mod.rs` (register pane), section-browser wiring in `crates/spur-tui/src/views/settings_tui.rs` (follow the existing tab/section registration pattern; zero-arg `TuiPane::new()` constructor per settings_tui.rs:29)
- Test: inline tests in `configure_section.rs` + `mcp_servers_tui.rs`

**Depends on:** task-1 (uses `ConfigPatch::McpServerUpsert/Remove`, `McpServerEntry`)

**Acceptance Criteria:**
- [ ] `/configure mcp` focuses the MCP section (parse_token test)
- [ ] Section lists entries (name, transport kind, enabled), supports add/edit/remove/toggle
- [ ] stdio form: command + args + env; http form: url + headers
- [ ] Saving emits `Action::ConfigSaveRequested { patch: ConfigPatch::McpServerUpsert { entry } }` (existing generic action, action.rs:136) — no new Action variant
- [ ] Footer renders "applies to next session"
- [ ] `scripts/spur-cargo test -p spur-tui` green; clippy clean

**Suggested Worker:** codex / rust-engineer / gpt-5.6-sol / xhigh

**Scope Boundary:**
- IN scope: files above
- OUT of scope: `action.rs` (reuse `ConfigSaveRequested`), `app/mod.rs` SAVE-APPLY handler, orchestrator input plumbing, `submit_router.rs`
- If the section browser requires edits beyond registering the new pane → emit `scope_drift`

**Implementation:**

- [ ] **Step 1: Failing tests**

```rust
// configure_section.rs tests
#[test]
fn mcp_token_focuses_mcp_section() {
    assert_eq!(parse_configure_arg("mcp"), (ConfigureSection::Mcp, None));
    assert_eq!(ConfigureSection::Mcp.as_str(), "mcp");
    assert_eq!(ConfigureSection::Mcp.list_label(), "MCP Servers");
    assert!(ConfigureSection::ALL.contains(&ConfigureSection::Mcp));
    assert_eq!(ConfigureSection::ALL.len(), 5);
}

// mcp_servers_tui.rs tests
#[test]
fn save_emits_config_save_requested_upsert() {
    let entry = test_http_entry("github"); // local helper building McpServerEntry
    let pane = McpServersPane::new();
    let action = pane.save_action_for(entry.clone());
    assert!(matches!(
        action,
        crate::action::Action::ConfigSaveRequested {
            patch: crate::spur_acp::config::ConfigPatch::McpServerUpsert { ref entry }
        } if *entry == test_http_entry("github")
    ));
}

#[test]
fn disabled_entry_renders_with_marker() {
    let mut pane = McpServersPane::new();
    pane.set_entries(vec![disabled_entry("ghost")]);
    let text = pane.render_snapshot();
    assert!(text.contains("ghost") && text.contains("disabled"));
}
```

- [ ] **Step 2:** `scripts/spur-cargo test -p spur-tui mcp` → FAIL (no `Mcp` variant / pane)

- [ ] **Step 3: Implement** — mirror `settings_tui.rs` structure: hold `Vec<McpServerEntry>` (from the app's live config snapshot), a small form state enum `Form { None, Stdio, Http }`, and on save construct the entry, run the same client-side checks the pane can do (non-empty name — full validation lives in `ConfigPatch::apply`, which reports errors back through the existing SAVE-APPLY error channel), and dispatch `Action::ConfigSaveRequested`. Remove/toggle map to `ConfigPatch::McpServerRemove { name }` / upsert with flipped `enabled`. Follow the pane rendering + event conventions of `settings_tui.rs` exactly (colors, keybindings for the section browser).

- [ ] **Step 4:** `scripts/spur-cargo test -p spur-tui && scripts/spur-cargo clippy -p spur-tui -- -D warnings`

- [ ] **Step 5:** Commit — `feat(spur-tui): task-3 /configure mcp section with SAVE-APPLY`

**Scope Drift Checkpoint:**
- TUI pane conventions differ from what's described → adapt within `views/`, but if `action.rs`/`app/mod.rs` changes seem required → emit `scope_drift` first (they should NOT be: `ConfigSaveRequested` is generic over `ConfigPatch`)

---

## Self-Review

1. **Spec coverage:** schema+patch+validation (Task 1), injection-after-fixed + enabled-only + workers-untouched (Task 2), section UI + SAVE-APPLY + next-session notice (Task 3). Reserved names, both transports, remove semantics — covered. Gap check: `EnvVariable` re-export handled in Task 2 as a scoped one-liner. ✔
2. **Placeholder scan:** no TBD/TODO; all code steps carry real code; the two "adjust if" notes name the exact fallback (`struct literal with meta: None`, `.name()` variant matching). ✔
3. **Type consistency:** `McpServerEntry`/`McpServerTransport`/`McpServersConfig` names identical across tasks; SDK builders match agent-client-protocol 1.2.0 sources. ✔
4. **DAG:** task-1 → {task-2, task-3}; no cycles; T2 ∥ T3. ✔
5. **beads compatibility:** unique IDs, explicit depends_on, reviewable acceptance criteria, scope boundaries with signal instructions. ✔
