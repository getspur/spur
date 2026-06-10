"""Tests for spur_app.errors."""
from __future__ import annotations

import pytest

from pathlib import Path

from spur_app.errors import (
    ArtifactPathError,
    EnvVarRequiredError,
    MissingCapabilityError,
    PortFileNotFoundError,
    PortManifestError,
    PortNotFoundError,
    SpurAppError,
)


def test_missing_capability_error_hierarchy():
    err = MissingCapabilityError(
        "ports",
        "SPUR_PORTS_ROOT not provisioned — declare capabilities.ports in spur-app.json",
    )
    assert isinstance(err, SpurAppError)
    assert err.capability == "ports"
    assert "SPUR_PORTS_ROOT" in str(err)
    assert "spur-app.json" in str(err)


def test_missing_capability_error_artifacts():
    err = MissingCapabilityError(
        "artifacts_dir",
        "SPUR_ARTIFACTS_DIR not provisioned — declare capabilities.artifacts_dir in spur-app.json",
    )
    assert err.capability == "artifacts_dir"
    assert "SPUR_ARTIFACTS_DIR" in str(err)


def test_port_not_found_error():
    err = PortNotFoundError("missing_port", ["sales", "spur-ad-capture"])
    assert isinstance(err, SpurAppError)
    assert err.port == "missing_port"
    assert "missing_port" in str(err)
    assert "sales" in str(err)
    assert "spur-ad-capture" in str(err)


def test_port_not_found_error_empty_available():
    err = PortNotFoundError("x", [])
    assert "(none)" in str(err)


def test_port_file_not_found_error():
    p = Path("/tmp/sales@v1.arrow")
    err = PortFileNotFoundError("sales", p)
    assert isinstance(err, SpurAppError)
    assert err.port == "sales"
    assert err.path == p
    assert isinstance(err.path, Path)
    assert "sales" in str(err)
    assert "sales@v1.arrow" in str(err)


def test_port_manifest_error():
    p = Path("/tmp/port-store/manifest.json")
    err = PortManifestError(p, "file not found")
    assert isinstance(err, SpurAppError)
    assert err.manifest_path == p
    assert err.reason == "file not found"
    assert "manifest.json" in str(err)
    assert "file not found" in str(err)


def test_env_var_required_error():
    err = EnvVarRequiredError("MY_VAR")
    assert isinstance(err, SpurAppError)
    assert err.name == "MY_VAR"
    assert "MY_VAR" in str(err)
    assert "spur-app.json" in str(err)


def test_artifact_path_error():
    err = ArtifactPathError("absolute paths not allowed")
    assert isinstance(err, SpurAppError)
