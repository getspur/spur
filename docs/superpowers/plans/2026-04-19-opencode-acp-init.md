# OpenCode ACP Initialization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Integrate the `opencode` Agent Client Protocol (ACP) server into Spur's `init` workflow so it is discoverable and configurable during agent onboarding.

**Architecture:** We will append a new agent definition block `opencode-acp` to `seed_agents.toml`. To support the onboarding UX, we will also add a corresponding install hint to `init.rs` and update the parallel integration test in `init_ux.rs` which enforces this contract.

**Tech Stack:** Rust, TOML.

---

### Task 1: Add OpenCode ACP to Seed Agents Configuration

**Files:**
- Modify: `crates/spur-acp/src/seed_agents.toml`

- [ ] **Step 1: Write the failing test**

```bash
# We rely on the existing contract test in init_ux.rs, which will fail if a seed agent is not present in its expected_names list.
cargo test -p spur-cli --test init_ux -- --exact install_hints_cover_all_seed_agents
```
Expected: PASS (Baseline before modifications).

- [ ] **Step 2: Implement minimal implementation**

Append the following TOML block to the end of `crates/spur-acp/src/seed_agents.toml`:

```toml

# ── opencode-acp ──────────────────────────────────────────────────
[[agents.entries]]
name = "opencode-acp"
command = "opencode"
args = ["acp"]
transport = "acp"
kind = "opencode-acp"
role = "both"
cost_tier = "medium"

[agents.entries.display]
handle = "opencode"

[agents.entries.commands]
dispatch = "prompt_text"

[agents.entries.permissions]
session_mode = "full-auto"
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p spur-cli --test init_ux -- --exact install_hints_cover_all_seed_agents`
Expected: FAIL with "seed agent `opencode-acp` has no INSTALL_HINTS entry".

- [ ] **Step 4: Commit**

```bash
git add crates/spur-acp/src/seed_agents.toml
git commit -m "feat(acp): register opencode-acp in seed_agents"
```

### Task 2: Update Initialization Command and Contract Test

**Files:**
- Modify: `crates/spur-cli/src/commands/init.rs`
- Modify: `crates/spur-cli/tests/init_ux.rs`

- [ ] **Step 1: Implement the installation hint**

In `crates/spur-cli/src/commands/init.rs`, add the `opencode-acp` tuple to the `INSTALL_HINTS` array:

```rust
pub const INSTALL_HINTS: &[(&str, &str)] = &[
    ("claude-code", "npm install -g @anthropic-ai/claude-code"),
    ("kiro", "brew install kiro-cli"),
    (
        "claude-code-acp",
        "npm install -g npx   # then re-run `spur init`",
    ),
    (
        "codex",
        "https://github.com/zed-industries/codex-acp/releases",
    ),
    ("codex-acp", "npx @zed-industries/codex-acp"),
    ("gemini", "npm install -g @google/gemini-cli"),
    ("opencode-acp", "npm install -g opencode"),
];
```

- [ ] **Step 2: Implement the contract test update**

In `crates/spur-cli/tests/init_ux.rs`, append `"opencode-acp"` to the `expected_names` array inside `install_hints_cover_all_seed_agents()`:

```rust
    let expected_names: &[&str] = &[
        "claude-code",
        "kiro",
        "claude-code-acp",
        "codex",
        "codex-acp",
        "gemini",
        "opencode-acp",
    ];
```

- [ ] **Step 3: Run test to verify it passes**

Run: `cargo test -p spur-cli --test init_ux -- --exact install_hints_cover_all_seed_agents`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-cli/src/commands/init.rs crates/spur-cli/tests/init_ux.rs
git commit -m "feat(cli): add opencode-acp onboarding installation hint"
```
