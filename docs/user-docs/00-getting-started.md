# Getting Started with Spur

Welcome to Spur! This guide will walk you through installing the Spur TUI, initializing your first workspace, and orchestrating your first agent-driven task.

## 1. Prerequisites & Installation

Before using Spur, you need to ensure you have the underlying backend AI agents installed, as Spur acts as the orchestrator (the "Brain") that delegates tasks to these tools.

### Prerequisites
* **Git Repository:** Spur is designed to run inside a git repository.
* **Backend Agents:** You must have at least one supported AI CLI agent installed and authenticated on your system (e.g., [Claude Code](https://docs.anthropic.com/en/docs/agents-and-tools/claude-code/overview) or Codex). Ensure the agent's executable (like `claude`) is available in your system's `$PATH`.

### Installation
Spur is a Rust-native application delivered conveniently via an npm wrapper package. You can install it globally or run it directly using `npx`.

**Install globally via npm:**
```bash
npm install -g @getspur/spur-cli
```

**Or run directly via npx:**
```bash
npx @getspur/spur-cli tui
```

## 2. Initialization

Spur keeps its configuration and session data isolated per repository in a `.spur` directory. 

To get started, navigate to the root of your project repository in your terminal and run:

```bash
spur init
```

This command sets up the necessary workspace environment, creating a `.spur/config.toml` file where you can customize agent routing, UI preferences, and workspace behaviors.

## 3. First Launch

Once initialized, start the Spur TUI by running:

```bash
spur
```
*(Or use `npx @getspur/spur-cli tui` if you didn't install it globally).*

You will be greeted by the Spur terminal user interface, which displays your active sessions, agent status, and conversation history.

> 🎥 **Video Placeholder:** Running spur init and starting the app.

## 4. First Steps

Now that Spur is running, it's time to orchestrate your first task!

1. **Create a Session:** If not already in an active session, follow the on-screen keybinds to start a new one.
2. **Enter a Prompt:** Use the input box at the bottom to type your request. Be as specific as you like. For example:
   * *"Add a dark mode toggle to the navigation bar."*
   * *"Refactor the authentication module to use the new database schema."*
   * *"Fix the memory leak in the worker pool."*
3. **Watch the Orchestration:** Once you submit the prompt, the **Brain** agent takes over. It will analyze your request, formulate an implementation plan, and delegate the execution to a **Worker** agent (like Claude Code). You can sit back and monitor the real-time execution, logs, and tool calls directly from the TUI.

> 🎥 **Video Placeholder:** Executing your first task and watching delegation.

Congratulations! You are now using Spur to manage and orchestrate AI development tasks in your workspace.
