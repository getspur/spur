# Session Management & Details

Spur keeps track of your ongoing and past conversations with agents through **Sessions**. You can switch between tasks, resume older conversations, and manage multiple lines of work without losing context.

This guide explains how to navigate your sessions using the **Session Picker** and how to understand the output in the **Session Detail** view.

## The Session Picker

> 🎥 **Video Placeholder:** [Demonstrate opening the Session Picker with Alt+S, navigating the list, using the search filter, and toggling the Preview Pane.]

The **Session Picker** is your hub for switching contexts. It displays a list of your recent sessions along with their current working directory (CWD), the brain/agent used, and the last time they were active.

**To open the Session Picker:**
- Press `Alt+s` from almost anywhere in the TUI.
- Type `/sessions` in the input bar.

### Navigation & Actions

When the Session Picker is open, use the following keys to navigate and manage your sessions:

| Key | Action |
| :--- | :--- |
| `j` / `k` or `Up` / `Down` | Navigate up and down the list of sessions. |
| `Enter` | Resume the highlighted session, or start a new one if `+ Start new session` is selected. |
| `n` | Instantly start a new session. |
| `/` | Open the search bar to filter sessions. |
| `p` | **Pin** the session. Pinned sessions stay at the top of the list. |
| `x` | **Archive** the session. Archived sessions are hidden from the default view. |
| `a` | Toggle the visibility of archived sessions. |
| `R` | **Rename** the session. An inline prompt will appear to type a new title. |
| `y` | **Yank (Copy)** the Session ID to your clipboard. |
| `P` | Toggle the **Preview Pane** (see below). |
| `Esc` | Return to the dashboard or previous view. |

### Search & Filtering
Press `/` to focus the search bar. You can type to filter the list based on the session's title, the initial intent (first message), the working directory, or the session ID. Press `Esc` or `Enter` to leave the search bar and return to navigating the filtered list.

### The Preview Pane
Pressing `P` (capital P) opens a detailed **Preview Pane** below the list for the currently highlighted session. It quickly summarizes:
- **Last**: The most recent message sent in the session.
- **Draft**: Any unsent text currently sitting in the composer for that session.
- **Intent**: The very first message that started the session.
- **Footer**: The CWD, the agent name, and the short ID.

---

## Session Detail & The ReAct Trace

When you select a session (or start a new one), you are taken to the **Session Detail** view. This is your primary workspace for interacting with the brain.

### The ReAct Trace Explained
Spur agents use a **ReAct (Reasoning and Acting)** loop. Instead of just giving you a final answer, the agent streams its internal thought process and tool execution in real-time. This is represented in the **Trace Pane**.

You will see several types of entries in the trace:
- **User Messages**: Your prompts and commands.
- **Thoughts (Think)**: The agent's internal reasoning ("I need to search the codebase for...").
- **Tool Calls & Observations (Observe)**: The agent executing commands, reading files, or delegating tasks, followed by the system's response.
- **Agent Messages**: The final response or question directed back to you.
- **Permissions**: Prompts asking you to authorize an action (e.g., modifying a file or running a shell command).

### Session Detail Shortcuts

While in the Session Detail view, you can manage the trace and your input using these shortcuts:

| Key | Action |
| :--- | :--- |
| `PageUp` / `PageDown` | Scroll the Trace Pane up and down. |
| `j` / `k` / `g` / `G` | Scroll the Trace Pane (only works when the input bar is empty). |
| `Ctrl+O` | Toggle collapse/expand for **Observe** entries (useful to hide long tool outputs). |
| `Alt+I` | Toggle the input bar between Vim and Emacs editing modes. |
| `Alt+m` | Toggle between "default" and "plan" session modes. |
| `Esc` | **Cancel an in-flight stream.** If the agent is currently thinking or streaming a response, pressing `Esc` will interrupt and stop it. |
| `y` / `n` / `a` | Answer pending **Permission** prompts (Yes, No, Always Allow). |

### Input History
Spur remembers what you've typed. 
- Use `Ctrl+P` (Previous) and `Ctrl+N` (Next) to cycle through your previously sent messages.
- Use `Ctrl+R` or `Alt+R` to open a searchable history popup.

## Drafts & Persistence

Spur automatically saves drafts. If you type a message into the input bar but navigate away (e.g., using `Alt+s` to open the Session Picker or switching to another session), your text is securely saved as a draft. 

When you return to that session, your draft will be exactly as you left it. If you attempt to start a new session while you have an active draft, Spur will show a warning banner to prevent accidental data loss.
ntal data loss.
