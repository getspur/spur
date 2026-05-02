# Configuration System

Spur uses a per-repository configuration approach to ensure that your agent environment is perfectly tuned for each specific project. When you run `spur init`, Spur scans your system for supported agents and generates a `.spur/config.toml` file in your repository's root.

This guide explains the structure of `.spur/config.toml` and how you can customize it to add new agents, change how the "Brain" delegates tasks, and configure agent permissions.

## The `.spur/config.toml` File

All settings for Spur live inside `.spur/config.toml`. 
*Note: Re-running `spur init` will overwrite this file with a new seed template, so it is recommended to edit it by hand once generated.*

### 1. Brain Framework Configuration

At the top of the file, you can configure how the "Brain" agent orchestrates tasks.

```toml
[brain.delegation]
# "v1" enables the advanced brain prompt (workers block, dispatch procedure).
# "legacy" uses the simpler, pre-framework prompt.
framework = "v1"
```

### 2. Agent Entries (`[[agents.entries]]`)

The core of the config file is the `agents.entries` array. Each entry defines an AI agent that Spur can communicate with. 

Here is an example of an agent configuration:

```toml
[[agents.entries]]
name = "claude-code-acp"
command = "npx"
args = ["--yes", "@agentclientprotocol/claude-agent-acp@0.26.0"]
transport = "acp"
role = "both"
cost_tier = "medium"
```

#### Key Fields:
*   **`name`**: A unique identifier for the agent in the config.
*   **`command`**: The executable command used to launch the agent (e.g., `npx`, `kiro-cli`, `codex`).
*   **`args`**: Command-line arguments passed to the agent on startup.
*   **`transport`**: How Spur communicates with the agent. 
    *   `acp`: Uses the standard Agent Client Protocol (JSON-RPC 2.0).
    *   `stream-json`: For tools that output streaming JSON but aren't strictly ACP.
    *   `cli-wrap`: Wraps standard CLI stdin/stdout tools.
*   **`role`**: Defines what this agent is allowed to do. 
    *   `brain`: Only acts as an orchestrator.
    *   `worker`: Only acts as an executor.
    *   `both`: Can be used as either.
*   **`cost_tier`**: Used by the Brain to make economic delegation choices (`low`, `medium`, `high`).

### 3. Display and Dispatch Settings

You can customize how the agent appears in the UI and how it receives commands.

```toml
[agents.entries.display]
handle = "claude" # Used for @mentions (e.g., @claude)

[agents.entries.commands]
dispatch = "prompt_text" # How the initial prompt is sent
```

### 4. Permissions and Auto-Approval

By default, many AI agents require user confirmation before running terminal commands or modifying files. You can configure Spur to automatically pass bypass flags.

```toml
[agents.entries.permissions]
# Example for Claude Code ACP bypass
session_mode = "bypassPermissions"

# Example for standard CLI bypass
# args = ["--dangerously-skip-permissions"]
# skip = true
```

### 5. Brain Delegation tuning

You can change how the Brain perceives a specific worker agent. By overriding the delegation descriptor, you tell the Brain what an agent is "good for" and what it should "avoid".

```toml
[agents.entries.delegation]
description = "A fast, inexpensive agent for simple refactoring."
good_for = ["Writing tests", "Updating documentation", "Simple refactors"]
avoid_for = ["Complex architectural changes", "Database migrations"]
```
*(If omitted, Spur uses built-in defaults for known agents).*

## Brain Skills

Spur ships with built-in `SKILL.md` files that instruct the Brain on how to delegate, review, and coordinate tasks. 

If you want to override these instructions for a specific project:
1. Create `.spur/skills/brain-delegation-{agent}/SKILL.md`.
2. Add your custom system prompts or procedures.
3. The Brain will prioritize your project-specific skill file over the built-in defaults.
