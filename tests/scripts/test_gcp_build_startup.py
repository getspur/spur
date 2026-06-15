import os
import subprocess
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
CONFIG_ENV = ROOT / "scripts" / "gcp-build" / "config.env"
STARTUP_SH = ROOT / "scripts" / "gcp-build" / "startup.sh"


def extract_wrapper(startup: str, name: str) -> str:
    marker = f"cat >/usr/local/bin/{name} <<'WRAPPER'\n"
    start = startup.index(marker) + len(marker)
    end = startup.index("\nWRAPPER", start)
    return startup[start:end] + "\n"


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


def test_c_wrappers_disable_working_directory_linemarkers_for_stable_sccache_keys():
    startup = STARTUP_SH.read_text()

    assert '[[ -n "${OUT_DIR:-}" ]]' in startup
    assert 'mapped+=("${arg#$OUT_DIR/}")' in startup
    assert 'mapped+=("-I${arg#-I$OUT_DIR/}")' in startup
    assert 'cd "$OUT_DIR"' in startup
    assert 'exec /usr/local/bin/sccache /usr/bin/cc -fno-working-directory "${mapped[@]}"' in startup
    assert 'exec /usr/local/bin/sccache /usr/bin/c++ -fno-working-directory "${mapped[@]}"' in startup


def test_c_wrapper_preserves_crate_relative_sources_when_out_dir_is_set(tmp_path):
    startup = STARTUP_SH.read_text()
    fake_sccache = tmp_path / "sccache"
    fake_sccache.write_text(
        "#!/bin/bash\n"
        "printf 'cwd=%s\\n' \"$PWD\"\n"
        "printf 'basedir=%s\\n' \"${SCCACHE_BASEDIR:-}\"\n"
        "printf 'args=%s\\n' \"$*\"\n"
    )
    fake_sccache.chmod(0o755)

    wrapper = tmp_path / "sccache-cc"
    wrapper.write_text(extract_wrapper(startup, "sccache-cc").replace("/usr/local/bin/sccache", str(fake_sccache)))
    wrapper.chmod(0o755)

    crate_dir = tmp_path / "crate"
    out_dir = tmp_path / "target" / "debug" / "build" / "zstd-sys" / "out"
    (crate_dir / "zstd" / "lib" / "common").mkdir(parents=True)
    out_dir.mkdir(parents=True)
    (crate_dir / "zstd" / "lib" / "common" / "debug.c").write_text("int x;\n")

    env = os.environ.copy()
    env["OUT_DIR"] = str(out_dir)
    result = subprocess.run(
        [
            str(wrapper),
            "-I",
            "zstd/lib",
            "-o",
            str(out_dir / "debug.o"),
            "-c",
            "zstd/lib/common/debug.c",
        ],
        cwd=crate_dir,
        env=env,
        text=True,
        capture_output=True,
    )

    assert result.returncode == 0, result.stderr
    assert result.stdout.splitlines() == [
        f"cwd={crate_dir}",
        f"basedir={crate_dir}",
        f"args=/usr/bin/cc -fno-working-directory -I zstd/lib -o {out_dir / 'debug.o'} -c zstd/lib/common/debug.c",
    ]


def test_c_wrapper_rewrites_out_dir_scoped_generated_sources(tmp_path):
    startup = STARTUP_SH.read_text()
    fake_sccache = tmp_path / "sccache"
    fake_sccache.write_text(
        "#!/bin/bash\n"
        "printf 'cwd=%s\\n' \"$PWD\"\n"
        "printf 'basedir=%s\\n' \"${SCCACHE_BASEDIR:-}\"\n"
        "printf 'args=%s\\n' \"$*\"\n"
    )
    fake_sccache.chmod(0o755)

    wrapper = tmp_path / "sccache-cc"
    wrapper.write_text(extract_wrapper(startup, "sccache-cc").replace("/usr/local/bin/sccache", str(fake_sccache)))
    wrapper.chmod(0o755)

    crate_dir = tmp_path / "crate"
    out_dir = tmp_path / "target" / "debug" / "build" / "libduckdb-sys" / "out"
    (out_dir / "duckdb" / "src" / "include").mkdir(parents=True)
    (out_dir / "duckdb" / "ub_src_planner.cpp").write_text("int x;\n")
    crate_dir.mkdir()

    env = os.environ.copy()
    env["OUT_DIR"] = str(out_dir)
    result = subprocess.run(
        [
            str(wrapper),
            "-I",
            str(out_dir / "duckdb" / "src" / "include"),
            "-o",
            str(out_dir / "planner.o"),
            "-c",
            str(out_dir / "duckdb" / "ub_src_planner.cpp"),
        ],
        cwd=crate_dir,
        env=env,
        text=True,
        capture_output=True,
    )

    assert result.returncode == 0, result.stderr
    assert result.stdout.splitlines() == [
        f"cwd={out_dir}",
        f"basedir={out_dir}",
        f"args=/usr/bin/cc -fno-working-directory -I duckdb/src/include -o planner.o -c duckdb/ub_src_planner.cpp",
    ]
