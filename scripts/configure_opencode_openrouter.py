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
