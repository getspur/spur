"""Tests for spur_app.env."""
from __future__ import annotations

import pytest

from spur_app.env import EnvAccessor
from spur_app.errors import EnvVarRequiredError


def test_get_returns_value(monkeypatch):
    monkeypatch.setenv("MY_TEST_VAR", "hello")
    acc = EnvAccessor()
    assert acc.get("MY_TEST_VAR") == "hello"


def test_get_returns_default_when_absent(monkeypatch):
    monkeypatch.delenv("MY_TEST_VAR", raising=False)
    acc = EnvAccessor()
    assert acc.get("MY_TEST_VAR") is None
    assert acc.get("MY_TEST_VAR", "fallback") == "fallback"


def test_require_returns_value(monkeypatch):
    monkeypatch.setenv("MY_TEST_VAR", "world")
    acc = EnvAccessor()
    assert acc.require("MY_TEST_VAR") == "world"


def test_require_raises_when_absent(monkeypatch):
    monkeypatch.delenv("MY_TEST_VAR", raising=False)
    acc = EnvAccessor()
    with pytest.raises(EnvVarRequiredError) as exc_info:
        acc.require("MY_TEST_VAR")
    err = exc_info.value
    assert err.name == "MY_TEST_VAR"
    assert "MY_TEST_VAR" in str(err)
    assert "spur-app.json" in str(err)


def test_path_returns_path_object(monkeypatch, tmp_path):
    monkeypatch.setenv("MY_PATH_VAR", str(tmp_path))
    acc = EnvAccessor()
    p = acc.path("MY_PATH_VAR")
    from pathlib import Path
    assert isinstance(p, Path)
    assert str(p) == str(tmp_path)


def test_path_raises_when_absent(monkeypatch):
    monkeypatch.delenv("MY_PATH_VAR", raising=False)
    acc = EnvAccessor()
    with pytest.raises(EnvVarRequiredError):
        acc.path("MY_PATH_VAR")
