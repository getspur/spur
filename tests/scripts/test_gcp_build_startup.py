import os
import subprocess
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
CONFIG_ENV = ROOT / "scripts" / "gcp-build" / "config.env"
INIT_SH = ROOT / "scripts" / "gcp-build" / "init.sh"
SPIN_SH = ROOT / "scripts" / "gcp-build" / "spin.sh"
STARTUP_SH = ROOT / "scripts" / "gcp-build" / "startup.sh"


def extract_wrapper(startup: str, name: str) -> str:
    marker = f"cat >/usr/local/bin/{name} <<'WRAPPER'\n"
    start = startup.index(marker) + len(marker)
    end = startup.index("\nWRAPPER", start)
    return startup[start:end] + "\n"


def extract_sbin_script(startup: str, name: str) -> str:
    marker = f"cat >/usr/local/sbin/{name} <<'SCRIPT'\n"
    start = startup.index(marker) + len(marker)
    end = startup.index("\nSCRIPT", start)
    return startup[start:end] + "\n"


def run_autoshutdown(tmp_path: Path, ps_output: str, who_output: str = "") -> subprocess.CompletedProcess[str]:
    startup = STARTUP_SH.read_text()
    targets = tmp_path / "targets"
    worktrees = targets / "worktrees"
    fake_shutdown = tmp_path / "shutdown"
    shutdown_args = tmp_path / "shutdown.args"
    fake_bin = tmp_path / "bin"
    fake_bin.mkdir()
    targets.mkdir(exist_ok=True)

    script = extract_sbin_script(startup, "spur-autoshutdown")
    script = script.replace("IDLE_MIN=__IDLE_MIN__", "IDLE_MIN=30")
    script = script.replace("TARGETS=/mnt/cargo/targets", f"TARGETS={targets}")
    script = script.replace("WORKTREES=/mnt/cargo/targets/worktrees", f"WORKTREES={worktrees}")
    script = script.replace("/sbin/shutdown", str(fake_shutdown))

    script_path = tmp_path / "spur-autoshutdown"
    script_path.write_text(script)
    script_path.chmod(0o755)

    (fake_bin / "ps").write_text(f"#!/bin/sh\ncat <<'EOF'\n{ps_output}EOF\n")
    (fake_bin / "ps").chmod(0o755)
    (fake_bin / "pgrep").write_text(
        "#!/bin/sh\n"
        "name=\"\"\n"
        "for arg in \"$@\"; do\n"
        "  case \"$arg\" in -*) ;; *) name=\"$arg\" ;; esac\n"
        "done\n"
        f"awk -v name=\"$name\" '$2 == name {{ print $1; found=1 }} END {{ exit(found ? 0 : 1) }}' <<'EOF'\n{ps_output}EOF\n"
    )
    (fake_bin / "pgrep").chmod(0o755)
    (fake_bin / "who").write_text(f"#!/bin/sh\ncat <<'EOF'\n{who_output}EOF\n")
    (fake_bin / "who").chmod(0o755)
    fake_shutdown.write_text(f"#!/bin/sh\nprintf '%s\\n' \"$*\" > {shutdown_args}\n")
    fake_shutdown.chmod(0o755)

    env = os.environ.copy()
    env["PATH"] = f"{fake_bin}:{env['PATH']}"
    return subprocess.run([str(script_path)], env=env, text=True, capture_output=True)


def test_builder_defaults_to_standard_16_lssd_for_local_cargo_cache():
    config = CONFIG_ENV.read_text()

    assert ': "${VM_MACHINE_TYPE:=c4d-standard-16-lssd}"' in config
    assert "CACHE_DISK_SIZE_GB" not in config
    assert "CACHE_DISK_TYPE" not in config


def test_init_only_provisions_durable_gcs_sccache_state():
    init = INIT_SH.read_text()

    assert "Ensuring GCS sccache bucket" in init
    assert "gcloud compute disks create" not in init
    assert "Ensuring cache disk" not in init


def test_spin_creates_lssd_vm_without_persistent_cache_disk_attachment():
    spin = SPIN_SH.read_text()

    assert '--machine-type="$VM_MACHINE_TYPE"' in spin
    assert '--disk="name=$CACHE_DISK,device-name=cargo-cache,mode=rw,boot=no,auto-delete=no"' not in spin
    assert "cache disk stays attached" not in spin


def test_autoshutdown_defaults_to_30_minutes_and_tracks_linkers():
    startup = STARTUP_SH.read_text()

    assert "IDLE_MINUTES=30" in startup
    assert "rust-lld" in extract_sbin_script(startup, "spur-autoshutdown")


def test_autoshutdown_keeps_fresh_cargo_process_active(tmp_path):
    result = run_autoshutdown(
        tmp_path,
        ps_output="123 cargo 1800 /mnt/cargo/rustup/bin/cargo test --workspace\n",
    )

    assert result.returncode == 1
    assert "active: build process running" in result.stdout
    assert not (tmp_path / "shutdown.args").exists()


def test_autoshutdown_treats_cargo_older_than_one_hour_as_stale(tmp_path):
    recent_target = tmp_path / "targets" / "debug" / "fresh-artifact"
    recent_target.parent.mkdir(parents=True)
    recent_target.write_text("fresh\n")

    result = run_autoshutdown(
        tmp_path,
        ps_output=(
            "123 cargo 3601 /mnt/cargo/rustup/bin/cargo test --workspace\n"
            "456 rustc 20 rustc --crate-name spur_core\n"
        ),
    )

    assert result.returncode == 0
    assert "stale: cargo process older than 60 min" in result.stdout
    assert "idle for 30+ min" in result.stdout
    assert (tmp_path / "shutdown.args").read_text().startswith("-h now SPUR autoshutdown")


def test_startup_mounts_local_ssd_as_cargo_cache():
    startup = STARTUP_SH.read_text()

    assert "CACHE_MNT=/mnt/cargo" in startup
    assert "CACHE_LABEL=cargo-cache" in startup
    assert "google-local-nvme-ssd-*" in startup
    assert 'mkfs.ext4 -F -L "$CACHE_LABEL" "$CACHE_DEV"' in startup
    assert "mount \"$CACHE_DEV\" \"$CACHE_MNT\"" in startup


def test_startup_makes_local_ssd_cache_writable_before_rustup_bootstrap():
    startup = STARTUP_SH.read_text()

    mount_idx = startup.index('mountpoint -q "$CACHE_MNT" || mount "$CACHE_DEV" "$CACHE_MNT"')
    chmod_idx = startup.index('chmod 1777 "$CACHE_MNT"')
    rustup_idx = startup.index('if [[ ! -x "$CACHE_MNT/cargo-home/bin/rustup" ]]')

    assert mount_idx < chmod_idx < rustup_idx


def test_sccache_uses_local_ssd_l1_before_gcs_l2():
    startup = STARTUP_SH.read_text()

    assert "export SCCACHE_MULTILEVEL_CHAIN=disk,gcs" in startup
    assert "export SCCACHE_MULTILEVEL_WRITE_ERROR_POLICY=l0" in startup
    assert "export SCCACHE_DIR=$CACHE_MNT/sccache/\\${USER:-builder}" in startup
    assert "export SCCACHE_CACHE_SIZE=50G" in startup
    assert "SCCACHE_RAM_MNT" not in startup
    assert "mount -t tmpfs" not in startup


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
