# OpenCode OpenRouter Configuration Design

## Purpose
To configure the OpenCode CLI globally to use OpenRouter as a primary model provider, while adhering to strict L9 security and isolation principles.

## Architecture: The Hermetic Named Provider
This design resolves namespace collisions and token leakage by separating authentication from routing. It uses a "Hybrid" model: securing credentials in the environment while registering explicit routing paths in the global OpenCode configuration.

### Component 1: Environment Security Layer
- **Location:** `~/.zshrc` (or equivalent shell profile)
- **Variable:** `OPENROUTER_API_KEY`
- **Rationale:** Prevents credential leakage into dotfile repositories and avoids polluting the global `OPENAI_API_KEY` namespace, ensuring other CLI tools remain unaffected.

### Component 2: Global JSON Registry
- **Location:** `~/.config/opencode/opencode.json`
- **Structure:**
  - We define a dedicated `openrouter` provider object rather than overriding the default `openai` block.
  - **Type:** `openai` (to utilize standard chat-completion protocol).
  - **Base URL:** `https://openrouter.ai/api/v1`
  - **API Key Injection:** Dynamically mapped to `${OPENROUTER_API_KEY}`.
  - **Default Model:** `anthropic/claude-3.5-sonnet` (or user preference).
  - **Telemetry Headers:** Injects `HTTP-Referer` and `X-Title` for optimal OpenRouter routing and analytics.

## Data Flow
1. User executes `opencode <prompt>`.
2. OpenCode reads `opencode.json` and defaults to the `openrouter` provider.
3. OpenCode reads `OPENROUTER_API_KEY` from the environment.
4. OpenCode multiplexes the request to `https://openrouter.ai/api/v1` using the specified model string and custom headers.

## Error Handling
- **Missing Token:** If `OPENROUTER_API_KEY` is not set, OpenCode will gracefully fail with an authorization error rather than falling back to an invalid `OPENAI_API_KEY`.
- **Namespace Collisions:** Eliminated. Other tools relying on `OPENAI_API_KEY` will continue to route directly to OpenAI.

## Testing Strategy
1. **Validation Check 1:** Verify the environment variable is exported correctly (`echo $OPENROUTER_API_KEY`).
2. **Validation Check 2:** Execute a basic `opencode` command and verify it hits OpenRouter (e.g., check OpenRouter activity logs or look for successful LLM response).
3. **Validation Check 3:** Execute an independent tool relying on `OPENAI_API_KEY` to ensure it was not affected by the configuration.