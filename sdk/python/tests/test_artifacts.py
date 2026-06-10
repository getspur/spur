"""Tests for spur_app.artifacts."""
from __future__ import annotations

import os
import tempfile
from pathlib import Path

import pytest

from spur_app.artifacts import ArtifactStore
from spur_app.errors import ArtifactPathError, MissingCapabilityError


def test_missing_capability_when_env_absent(monkeypatch):
    monkeypatch.delenv("SPUR_ARTIFACTS_DIR", raising=False)
    with pytest.raises(MissingCapabilityError) as exc_info:
        ArtifactStore()
    err = exc_info.value
    assert err.capability == "artifacts_dir"
    assert "SPUR_ARTIFACTS_DIR" in str(err)
    assert "spur-app.json" in str(err)


def test_path_returns_path_under_root(tmp_path):
    store = ArtifactStore(root=tmp_path)
    p = store.path("output.mp4")
    assert p.parent == tmp_path


def test_path_creates_parent_dirs(tmp_path):
    store = ArtifactStore(root=tmp_path)
    p = store.path("renders/sub/output.mp4")
    assert p.parent.exists()


def test_path_returns_absolute(tmp_path):
    store = ArtifactStore(root=tmp_path)
    p = store.path("out.txt")
    assert p.is_absolute()


def test_path_rejects_absolute_input(tmp_path):
    store = ArtifactStore(root=tmp_path)
    with pytest.raises(ArtifactPathError, match="relative"):
        store.path("/etc/passwd")


def test_path_rejects_dotdot_escape(tmp_path):
    store = ArtifactStore(root=tmp_path)
    with pytest.raises(ArtifactPathError, match="escapes"):
        store.path("../outside")


def test_path_dotdot_within_root_allowed(tmp_path):
    """A ..  that stays inside the root is fine."""
    store = ArtifactStore(root=tmp_path)
    (tmp_path / "sub").mkdir()
    p = store.path("sub/../out.txt")
    # resolves to root/out.txt — still inside root
    assert p == tmp_path / "out.txt"


def test_root_property(tmp_path):
    store = ArtifactStore(root=tmp_path)
    assert store.root == tmp_path.resolve()


def test_env_var_used_when_no_root(monkeypatch, tmp_path):
    monkeypatch.setenv("SPUR_ARTIFACTS_DIR", str(tmp_path))
    store = ArtifactStore()
    p = store.path("a.txt")
    assert str(tmp_path) in str(p)
