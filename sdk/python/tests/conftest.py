"""Shared pytest configuration.

Re-exports ``fake_port_store`` so tests can receive it as a parameter without
importing it explicitly in each test module.
"""
from spur_app.testing import fake_port_store  # noqa: F401
