# OpenCode OpenRouter Configuration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement an automated configuration tool that idempotently applies the Hermetic Named Provider Architecture to `~/.config/opencode/opencode.json` and injects `OPENROUTER_API_KEY` into the user's shell profile.

**Architecture:** We will build a pure Python script `scripts/configure_opencode_openrouter.py` that loads, safely parses, injects the `openrouter` provider routing block, and saves the configuration. This ensures repeatable, version-controlled setups. The script will also output shell configuration instructions.

**Tech Stack:** Python 3 (standard library only for maximum portability: `json`, `os`, `pathlib`), pytest.

---

### Task 1: Setup Configuration Script Structure and Basic Tests

**Files:**
- Create: `tests/scripts/test_configure_opencode.py`
- Create: `scripts/configure_opencode_openrouter.py`

- [ ] **Step 1: Write the failing test**

```python
# tests/scripts/test_configure_opencode.py
import pytest
import os
import json
from pathlib import Path
from scripts.configure_opencode_openrouter import update_opencode_config

def test_update_opencode_config_creates_new_file(tmp_path):
    mock_config_path = tmp_path / "opencode.json"
    update_opencode_config(str(mock_config_path))
    
    assert mock_config_path.exists()
    
    with open(mock_config_path, 'r') as f:
        config = json.load(f)
        
    assert config.get("defaultProvider") == "openrouter"
    assert "openrouter" in config.get("providers", {})
    assert config["providers"]["openrouter"]["type"] == "openai"
```

- [ ] **Step 2: Run test to verify it fails**

Run: `python3 -m pytest tests/scripts/test_configure_opencode.py -v`
Expected: FAIL with "ModuleNotFoundError: No module named 'scripts.configure_opencode_openrouter'" or similar.

- [ ] **Step 3: Write minimal implementation**

```python
# scripts/configure_opencode_openrouter.py
import json
import os
from pathlib import Path

def get_openrouter_provider_block():
    return {
        "type": "openai",
        "apiBase": "https://openrouter.ai/api/v1",
        "apiKey": "${OPENROUTER_API_KEY}",
        "defaultModel": "anthropic/claude-3.5-sonnet",
        "headers": {
            "HTTP-Referer": "https://github.com/obra/superpowers",
            "X-Title": "OpenCode-L9-Workspace"
        }
    }

def update_opencode_config(config_path_str: str):
    config_path = Path(config_path_str)
    
    config = {}
    if config_path.exists():
        with open(config_path, 'r') as f:
            try:
                config = json.load(f)
            except json.JSONDecodeError:
                pass
                
    if "providers" not in config:
        config["providers"] = {}
        
    config["providers"]["openrouter"] = get_openrouter_provider_block()
    config["defaultProvider"] = "openrouter"
    
    config_path.parent.mkdir(parents=True, exist_ok=True)
    with open(config_path, 'w') as f:
        json.dump(config, f, indent=2)

if __name__ == "__main__":
    default_path = Path.home() / ".config" / "opencode" / "opencode.json"
    update_opencode_config(str(default_path))
    print(f"Updated OpenCode configuration at {default_path}")
```

- [ ] **Step 4: Run test to verify it passes**

Run: `python3 -m pytest tests/scripts/test_configure_opencode.py -v`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add tests/scripts/test_configure_opencode.py scripts/configure_opencode_openrouter.py
git commit -m "feat: implement OpenRouter OpenCode JSON configuration tool"
```

### Task 2: Implement Existing Provider Protection

**Files:**
- Modify: `tests/scripts/test_configure_opencode.py`
- Modify: `scripts/configure_opencode_openrouter.py`

- [ ] **Step 1: Write test for existing config retention**

```python
# append to tests/scripts/test_configure_opencode.py

def test_update_opencode_config_retains_existing_providers(tmp_path):
    mock_config_path = tmp_path / "opencode.json"
    existing_data = {
        "providers": {
            "openai": {
                "type": "openai",
                "apiKey": "test"
            }
        }
    }
    with open(mock_config_path, 'w') as f:
        json.dump(existing_data, f)
        
    update_opencode_config(str(mock_config_path))
    
    with open(mock_config_path, 'r') as f:
        config = json.load(f)
        
    assert "openai" in config["providers"]
    assert config["providers"]["openai"]["apiKey"] == "test"
```

- [ ] **Step 2: Run test to verify it passes**

Run: `python3 -m pytest tests/scripts/test_configure_opencode.py -v`
Expected: PASS (because our implementation in Task 1 used dictionary assignment instead of overwriting, but TDD validates our assumption).

- [ ] **Step 3: Commit**

```bash
git add tests/scripts/test_configure_opencode.py
git commit -m "test: verify OpenCode configuration tool retains existing provider blocks"
```

### Task 3: Apply the Configuration and Shell Environment

**Files:**
- Modify: `~/.zshrc` (or equivalent shell profile)
- Run: `scripts/configure_opencode_openrouter.py`

- [ ] **Step 1: Run the JSON configuration tool**

Run: `python3 scripts/configure_opencode_openrouter.py`
Expected: "Updated OpenCode configuration at /Users/.../.config/opencode/opencode.json"

- [ ] **Step 2: Verify JSON changes locally**

Run: `cat ~/.config/opencode/opencode.json | grep -A 5 -B 1 "openrouter"`
Expected: Prints the newly injected `openrouter` provider block with the explicit routing.

- [ ] **Step 3: Inject the shell environment variable**

Run: `echo '\n# OpenCode Hermetic OpenRouter Config\nexport OPENROUTER_API_KEY="sk-or-v1-YOUR_KEY_HERE"' >> ~/.zshrc`
Expected: Command succeeds quietly.

- [ ] **Step 4: Verify shell injection locally**

Run: `tail -n 3 ~/.zshrc`
Expected: Prints the newly appended `export OPENROUTER_API_KEY...` lines.

- [ ] **Step 5: Load Environment**

Run: `source ~/.zshrc`
Expected: Environment loaded. (Note: in automated shell, this won't persist across sessions, so inform the user manually).