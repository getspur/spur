from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
CONFIG_ENV = ROOT / "scripts" / "gcp-build" / "config.env"
STARTUP_SH = ROOT / "scripts" / "gcp-build" / "startup.sh"


def test_builder_defaults_to_standard_16_for_linker_memory_headroom():
    config = CONFIG_ENV.read_text()

    assert ': "${VM_MACHINE_TYPE:=c4d-standard-16}"' in config


def test_autoshutdown_defaults_to_30_minutes_and_tracks_linkers():
    startup = STARTUP_SH.read_text()

    assert "IDLE_MINUTES=30" in startup
    assert "pgrep -x rust-lld" in startup


def test_sccache_uses_ram_l1_before_gcs_l2():
    startup = STARTUP_SH.read_text()

    assert "SCCACHE_RAM_MNT=/mnt/sccache-ram" in startup
    assert "mount -t tmpfs -o size=16G,mode=1777 tmpfs \"$SCCACHE_RAM_MNT\"" in startup
    assert "export SCCACHE_MULTILEVEL_CHAIN=disk,gcs" in startup
    assert "export SCCACHE_MULTILEVEL_WRITE_ERROR_POLICY=l0" in startup
    assert "export SCCACHE_DIR=$SCCACHE_RAM_MNT/\\${USER:-builder}" in startup
    assert "export SCCACHE_CACHE_SIZE=15G" in startup
