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
