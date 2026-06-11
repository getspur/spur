from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
BUILD_SH = ROOT / "scripts" / "gcp-build" / "build.sh"
VITEST_CONFIG = ROOT / "crates" / "spur-notebook" / "jute-notebook" / "vitest.config.ts"


def test_remote_pnpm_uses_external_virtual_store_not_node_modules_symlink():
    script = BUILD_SH.read_text()
    normalized = script.replace('\\"', '"').replace("\\$", "$")

    assert "REMOTE_PNPM_VIRTUAL_STORE=" in script
    assert '--virtual-store-dir "$pnpm_virtual_store" install' in normalized
    assert 'ln -s "$pnpm_node_modules" "$link"' not in normalized
    assert "SPUR_REMOTE_PNPM_VIRTUAL_STORE=1" in normalized
    assert 'pnpm --dir "$frontend_dir"$PNPM_ARGS_ESCAPED' in normalized


def test_vitest_allows_remote_pnpm_virtual_store_only_on_builder():
    config = VITEST_CONFIG.read_text()

    assert "SPUR_REMOTE_PNPM_VIRTUAL_STORE" in config
    assert '"/mnt/cargo"' in config
