# AWS Mixed-path C/C++ sccache Wrapper Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make AWS Linux distribution builds support C/C++ crates that combine crate-relative sources with generated headers under Cargo's `$OUT_DIR`.

**Architecture:** Keep existing sccache normalization for generated sources, but select it only when the `-c` source operand is beneath `$OUT_DIR`. Cover the AWS provisioning template with an executable wrapper regression, install the corrected wrapper on the active builder, and rerun the Linux distribution path.

**Tech Stack:** Bash compiler wrappers, Python `unittest`, Cargo/cc-rs, AWS cloud-build helpers

---

### Task 1: Add the failing AWS mixed-path wrapper regression

**Files:**
- Create: `scripts/cloud-build/test/test_sccache_wrapper_paths.py` in `getspur/spur-notebook`

- [ ] **Step 1: Create the executable wrapper test**

```python
import os
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[3]
STARTUP = ROOT / "scripts" / "cloud-build" / "startup-aws.sh"


def extract_wrapper(startup: str, name: str) -> str:
    marker = f"cat >/usr/local/bin/{name} <<'WRAPPER'\n"
    start = startup.index(marker) + len(marker)
    end = startup.index("\nWRAPPER", start)
    return startup[start:end] + "\n"


class SccacheWrapperPathTests(unittest.TestCase):
    def test_out_dir_include_preserves_crate_relative_source(self) -> None:
        for wrapper_name, compiler in (("sccache-cc", "cc"), ("sccache-cxx", "c++")):
            with self.subTest(wrapper=wrapper_name), tempfile.TemporaryDirectory() as raw_tmp:
                tmp = Path(raw_tmp)
                crate_dir = tmp / "crate"
                out_dir = tmp / "target" / "release" / "build" / "libdbus-sys" / "out"
                source = crate_dir / "vendor" / "dbus" / "dbus.c"
                source.parent.mkdir(parents=True)
                source.write_text("int dbus_test;\n")
                (out_dir / "include").mkdir(parents=True)

                fake_sccache = tmp / "sccache"
                fake_sccache.write_text(
                    "#!/bin/bash\n"
                    "printf 'cwd=%s\\n' \"$PWD\"\n"
                    "printf 'basedir=%s\\n' \"${SCCACHE_BASEDIR:-}\"\n"
                    "printf 'args=%s\\n' \"$*\"\n"
                )
                fake_sccache.chmod(0o755)

                wrapper = tmp / wrapper_name
                wrapper.write_text(
                    extract_wrapper(STARTUP.read_text(), wrapper_name).replace(
                        "/usr/local/bin/sccache", str(fake_sccache)
                    )
                )
                wrapper.chmod(0o755)

                env = os.environ.copy()
                env["OUT_DIR"] = str(out_dir)
                result = subprocess.run(
                    [
                        str(wrapper),
                        "-I",
                        str(out_dir / "include"),
                        "-o",
                        str(out_dir / "dbus.o"),
                        "-c",
                        "vendor/dbus/dbus.c",
                    ],
                    cwd=crate_dir,
                    env=env,
                    text=True,
                    capture_output=True,
                )

                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(
                    result.stdout.splitlines(),
                    [
                        f"cwd={crate_dir}",
                        f"basedir={crate_dir}",
                        f"args=/usr/bin/{compiler} -fno-working-directory "
                        f"-I {out_dir / 'include'} -o {out_dir / 'dbus.o'} "
                        "-c vendor/dbus/dbus.c",
                    ],
                )


if __name__ == "__main__":
    unittest.main()
```

- [ ] **Step 2: Run the regression and verify RED**

```bash
python3 scripts/cloud-build/test/test_sccache_wrapper_paths.py
```

Expected: failure showing `cwd=<out_dir>` rather than `cwd=<crate_dir>`. This proves an OUT_DIR include incorrectly activates directory switching for a crate-relative source.

- [ ] **Step 3: Commit only the failing test**

```bash
git add scripts/cloud-build/test/test_sccache_wrapper_paths.py
git diff --cached --check
git commit -m "test(scripts): bd-2s0ef cover mixed sccache source paths"
```

### Task 2: Narrow AWS OUT_DIR classification to the compiled source

**Files:**
- Modify: `scripts/cloud-build/startup-aws.sh` in `getspur/spur-notebook`

- [ ] **Step 1: Update both C and C++ wrapper classifiers**

Replace each three-condition classifier with:

```bash
prev=""
for arg in "$@"; do
    if [[ "$prev" == "-c" && "$arg" == "$OUT_DIR"/* ]]; then
        out_dir_scoped=1
        break
    fi
    prev="$arg"
done
```

Do not change path mapping, the directory switch, or fallback behavior.

- [ ] **Step 2: Run the focused regression and provider contract**

```bash
python3 scripts/cloud-build/test/test_sccache_wrapper_paths.py
bash scripts/cloud-build/test/provider-contract.sh
```

Expected: both exit 0. The provider contract must continue to confirm all existing AWS provisioning invariants.

- [ ] **Step 3: Inspect and commit only the classifier hunks**

```bash
git diff -- scripts/cloud-build/startup-aws.sh
git add -p scripts/cloud-build/startup-aws.sh
git diff --cached --check
git diff --cached --name-only
git commit -m "fix(scripts): bd-2s0ef preserve mixed sccache source paths"
```

The pre-existing unrelated edit to the same file must remain unstaged.

### Task 3: Verify the active AWS Linux distribution path

**Files:**
- No tracked file changes

- [ ] **Step 1: Install the committed wrappers on `spur-builder`**

From the `spur-notebook` implementation worktree, run:

```bash
for wrapper_name in sccache-cc sccache-cxx; do
    payload="$(python3 - "$wrapper_name" <<'PY'
import base64
import sys
from pathlib import Path

name = sys.argv[1]
startup = Path("scripts/cloud-build/startup-aws.sh").read_text()
marker = f"cat >/usr/local/bin/{name} <<'WRAPPER'\n"
start = startup.index(marker) + len(marker)
end = startup.index("\nWRAPPER", start)
wrapper = startup[start:end] + "\n"
print(base64.b64encode(wrapper.encode()).decode())
PY
)"
    (
        cd scripts/cloud-build
        export SPUR_CLOUD=aws-my
        SCRIPT_DIR="$PWD"
        log() { echo "[wrapper-install] $*" >&2; }
        source ./config.env
        source ./provider-aws-my.sh
        provider_remote_ssh --command="printf '%s' '$payload' | base64 -d | sudo tee /usr/local/bin/$wrapper_name >/dev/null && sudo chmod 0755 /usr/local/bin/$wrapper_name"
    )
done

(
    cd scripts/cloud-build
    export SPUR_CLOUD=aws-my
    SCRIPT_DIR="$PWD"
    log() { echo "[wrapper-verify] $*" >&2; }
    source ./config.env
    source ./provider-aws-my.sh
    provider_remote_ssh --command="sed -n '1,35p' /usr/local/bin/sccache-cc; sed -n '1,35p' /usr/local/bin/sccache-cxx"
)
```

Expected: both installed wrappers contain only the source check
`[[ "$prev" == "-c" && "$arg" == "$OUT_DIR"/* ]]`; neither wrapper classifies
`-I` arguments.

- [ ] **Step 2: Re-run the original native Linux failure**

From SPUR:

```bash
scripts/spur-cargo build --release -p spur-cli
```

Expected: exit 0, with no missing `$OUT_DIR/./vendor/dbus/*.c` inputs from `libdbus-sys`.

- [ ] **Step 3: Re-run distribution assembly**

```bash
env -u SPUR_REMOTE target/debug/xtask dist
```

Expected: both Linux legs build and fetch successfully. If a macOS or Windows leg fails independently, preserve that evidence and report it separately.

- [ ] **Step 4: Run final tracked checks**

```bash
python3 scripts/cloud-build/test/test_sccache_wrapper_paths.py
bash scripts/cloud-build/test/provider-contract.sh
git diff --check
git status --short
```

Confirm only pre-existing unrelated changes remain uncommitted.

### Task 4: Record the outcome

**Files:**
- No tracked file changes

- [ ] **Step 1: Review the test and fix commits**

Verify the failing-test commit precedes the fix commit and neither commit contains unrelated paths.

- [ ] **Step 2: Record completion in beads**

Add a `[[spur-audit v1]]` completion comment to `bd-2s0ef` with the `spur-notebook` commit IDs and Linux dist verification summary. Close the issue only after the implementation and verification evidence are complete.
