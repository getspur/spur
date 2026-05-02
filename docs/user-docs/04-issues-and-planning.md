# Issues & Planning

Spur TUI provides powerful built-in tools for visualizing and managing your project's issues, dependencies, and execution plans. Rather than context-switching between your terminal and a web browser, you can inspect requirements, trace blockers, and track agent progress directly from the TUI.

## Issue Browser

The Issue Browser is your central hub for exploring tasks and epics. It synchronizes with your connected project management tracker (like GitHub, Linear, Plane, or local Beads) and displays issues alongside their metadata.

### Navigation and Detail Views

*   **`j` / `k`** or **Up / Down**: Navigate through the list of tracked issues.
*   **`g` / `G`**: Jump to the top or bottom of the list.
*   **`Enter`**: Open the detail pane for the currently selected issue.
*   **`Esc`**: Close the detail pane (or exit the Issue Browser if no detail pane is open).
*   **`PgUp` / `PgDn`**: Scroll through long issue descriptions or large graphs.

The detail pane has two modes: **Text Mode** (which shows the description, labels, and metadata) and **Graph Mode** (which maps out dependencies).

*   **`v`**: Toggle between Text Mode and Graph Mode for the selected issue.

### Managing Work

You can trigger agent actions or update issue states directly from the browser:

*   **`W` (Work on)**: Starts a new worker session to resolve the selected issue. 
*   **`E` (Execute Epic)**: Dispatches a planner agent to execute an entire epic. This option will show a confirmation modal before kicking off the workload.
*   **`r`**: Refresh the issue list from the upstream source.

**Quick Status Updates:**
*   **`o`**: Mark as Open
*   **`w`**: Mark as In Progress
*   **`b`**: Mark as Blocked
*   **`x` / `d`**: Mark as Closed

## Issue Dependency Graphs

When you press **`v`** while viewing an issue, the TUI switches to **Graph Mode**. The graph pane visualizes the dependency tree (what the current issue blocks, or is blocked by) using a visual depth-first search (DFS) tree.

The tree helps you identify execution order and potential circular dependencies:

*   **Legend**: 
    *   `○` Open
    *   `●` In Progress
    *   `!` Blocked
    *   `✓` Closed
*   **Cycles**: If an issue forms a circular dependency loop, it will be marked with a `↻ cycle` indicator.

If a graph has more nodes than can fit on your screen, use `PgUp` and `PgDn` to scroll, guided by the "↓ X more dependencies" hint at the bottom.

## Plan Inspector

When you ask the agent to execute a complex task or an epic, it generates a multi-stage **Plan**. The **Plan Inspector** provides a visual kanban-style board mapping the lifecycle of these tasks.

### The Board View

By default, on wider terminals, the Plan Inspector displays a side-by-side view:
*   **Stages (Lanes)**: The plan is broken down into sequential stages. All tasks in Stage 0 must complete before Stage 1 begins, and so on.
*   **Header**: Shows the plan's overall status (`RUNNING`, `APPROVED`, `FAILED`, etc.), a progress gauge (e.g., `4 / 10 done`), and the current action the brain is taking.
*   **Task Detail**: The right pane shows the execution logs, summaries, and agent output for the specifically highlighted task.

*(On narrower terminals, the lanes collapse into a stacked list to preserve readability.)*

### Navigating the Plan Inspector

*   **`h` / `l`** or **Left / Right**: Move horizontally between plan stages (lanes).
*   **`j` / `k`** or **Down / Up**: Select individual tasks within the current stage.
*   **`g` / `G`**: Jump to the first or last task in the current stage.
*   **`Esc`** or **`Alt-p`**: Close the Plan Inspector and return to the previous view.

### Task States & Meta Chips

Each task in the board shows a badge indicating its execution state:
*   `[QUE]`: Pending / Queued
*   `[RDY]`: Ready to run
*   `[RUN]`: Dispatched to a worker
*   `[REV]`: Awaiting review
*   `[PAS]`: Approved / Completed
*   `[ERR]`: Failed
*   `[REJ]`: Rejected (needs rework)
*   `[SKP]`: Cancelled / Skipped
*   `[SUP]`: Superseded
*   `BLOCKED`: Blocked by a setup conflict or another task.

Next to the task title, you may see **Meta Chips** providing live context:
*   **Live Worker** (e.g., `codex:run`): Displays the specific agent model and phase currently handling the task.
*   **Dependencies** (e.g., `↑T1`): Indicates which task IDs this task relies on.
*   **Retries** (e.g., `retry 2/3`): Shows if a task failed previously and is being re-attempted.
*   **Conflicts**: Explicit warnings if setup overlays conflicted (e.g., `2 files conflict with T1`).

## Mermaid Diagram Visualization

> 🎥 **Video Placeholder:** [Show an agent generating a Mermaid block, the placeholder transforming to the ready state, and pressing Alt-v to render the visual graph inline.]

Spur TUI natively supports **Mermaid diagrams** within issue descriptions, chat messages, and plan details. Rather than dumping raw text, Spur renders them into actual visual graphs directly inside your terminal window using a built-in rasterizer (`resvg` / `tiny-skia`).

When the TUI encounters a Mermaid code block, you'll see a placeholder while it generates:
*   `[⏳ mermaid #1 rendering…]` (In progress)
*   `[⚠ mermaid #1 error]` (Failed to parse)
*   `[📊 mermaid #1 · press Alt-v to view]` (Ready)

Once ready, press **`Alt-v`** to toggle the inline visual display of the diagram. The TUI intelligently maps the scale of the image to the layout of your terminal pane so text remains crisp and readable.
