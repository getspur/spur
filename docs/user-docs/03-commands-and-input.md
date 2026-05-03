# Commands & Input

The Spur TUI is designed to be completely keyboard-driven. At the heart of your interaction is the **Input Bar** (the composer) located at the bottom of the screen, along with powerful autocompletion pickers and a universal command palette.

This guide covers how to enter text, use editing modes, trigger commands, and navigate the application efficiently.

## The Input Bar

The Input Bar is where you type your messages to the AI. It features a status indicator in its top border to tell you what the application is currently doing:

*   **`● INSERT`**: You are in the default text-entry mode.
*   **Spinners**: When the AI is processing your input, you'll see animated spinners like Braille dots (Thinking), pulsing blocks (Streaming), or crawling dots (Connecting).

### Typing & Submitting

*   **Submit**: Press `Enter` to send your message to the agent.
*   **Insert a Newline**: Because `Enter` submits the message, use `Alt+Enter` or `Ctrl+J` to type a manual line break (newline) inside the input bar.

### Pasting Large Text

When you paste multiple lines of text into the input bar, Spur automatically intercepts the "paste burst". To prevent massive walls of text from taking up your entire screen, Spur compresses multi-line pastes into a single placeholder, such as:

`[Paste #1 · 15 lines]`

This placeholder acts as a single, protected block. You can type around it and move your cursor over it seamlessly. When you press `Enter`, the full pasted text is automatically expanded and sent to the agent.

## Editing Modes & Shortcuts

Spur supports two editing modes: **Emacs** (the default) and **Vim**. You can set your preference in your `config.toml` file under the editor settings.

### Emacs Mode (Default)

If you use default settings, the input bar uses standard Emacs-style readline shortcuts:

*   **`Ctrl+P` / `Ctrl+N`**: Navigate backward and forward through your input history.
*   **`Ctrl+U`**: Delete from the cursor to the start of the line.
*   **`Ctrl+K`**: Delete from the cursor to the end of the line.
*   **`Ctrl+W`**: Delete the word immediately behind the cursor.
*   **`Ctrl+A` / `Home`**: Move to the beginning of the line.
*   **`Ctrl+E` / `End`**: Move to the end of the line.

### Vim Mode

If you've configured Vim mode, the input bar provides a fully featured modal editing experience:
*   **Normal Mode** (`▣ VIM·NORMAL`): Use `h`, `j`, `k`, `l`, `w`, `b`, `e`, `0`, `$`, etc., to navigate. Standard operators like `d` (delete), `c` (change), and `y` (yank) work as expected.
*   **Insert Mode** (`● VIM·INSERT`): Press `i`, `a`, `o`, `I`, `A`, or `O` to enter Insert Mode. Press `Esc` to return to Normal Mode.
*   **Visual Mode** (`▦ VIM·VISUAL`): Press `v` or `V` to highlight text for deletion or copying.

## Mentions (`@`)

To provide context to the agent, you can "mention" specific files or other worker agents.

> 🎥 **Video Placeholder:** [Show typing `@` to trigger the mention picker, fuzzy searching for a file, hitting enter, and seeing it turn into a Protected Atom.]

Type `@` anywhere in the input bar to open the **Mention Picker**.
*   **Empty Query (`@`)**: Shows pinned worker agents at the top, followed by a list of files in your workspace.
*   **Typing to Filter (`@Cargo`)**: The picker uses fuzzy matching to instantly narrow down the list.

### Protected Atoms

When you select a mention from the picker and press `Tab` or `Enter`, it is inserted into your input as a **Protected Atom** (e.g., `@Cargo.toml`). 
*   Visually, atoms are highlighted in light blue and underlined.
*   Functionally, they act as a single character. Your cursor skips over them entirely, and pressing `Backspace` immediately deletes the whole atom. You cannot accidentally make a typo inside an inserted file path.

## Slash Commands (`/`)

Slash commands are special directives that tell Spur or the current agent to perform an action.

Type `/` to open the **Command Picker**.
*   This lists available commands, like `/help`, `/clear`, or `/model`.
*   Some commands are native to Spur (marked `⟨spur⟩`), while others are dynamically provided by the agent you are talking to (marked `⟨codex⟩`, `⟨gemini⟩`, etc.).
*   **Arguments**: Some commands require extra input. For example, typing `/model ` (with a trailing space) will automatically open a secondary picker to let you choose from the agent's available models.

### Interacting with Pickers

Whenever a picker overlay is open (for Mentions or Slash commands):
*   **`Up` / `Down`**: Move the selection cursor up and down.
*   **`Tab` / `Enter`**: Accept the currently highlighted option.
*   **`Esc`**: Dismiss the picker without inserting anything.

## The Universal Palette (`Ctrl+K`)

> 🎥 **Video Placeholder:** [Show opening the palette with Ctrl+K, typing to filter across categories, and hitting Enter to jump to a new view or session.]

For global navigation, Spur provides a Universal Command Palette that you can trigger from anywhere in the application.

*   Press **`Ctrl+K`** to open the palette.
*   The palette lets you instantly jump between different parts of the application using fuzzy search.

The palette groups results by category, easily identifiable by their badges:
*   `@` **Views**: Navigate to different screens or layouts.
*   `>` **Commands**: Execute global actions.
*   `$` **Sessions**: Jump directly into another active chat session.
*   `!` **Workers**: Switch focus to a specific worker agent.

Just start typing to filter, use arrow keys to select your destination, and hit `Enter` to jump right there.
