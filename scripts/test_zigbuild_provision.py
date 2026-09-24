#!/usr/bin/env python3
"""Exercise the optional cross-compiled Lance kernel without a cloud VM."""

import os
from pathlib import Path
import subprocess
import shutil
import tempfile
import tarfile
import unittest


SCRIPT = Path(__file__).with_name("zigbuild-provision-vm.sh")


class OptionalLanceKernelTests(unittest.TestCase):
    def run_kernel_step(self, cached_source=False, compiler_exit=0):
        source = SCRIPT.read_text()
        step = source.split('echo "== lance dist_table AVX-512 kernel (x86_64) =="', 1)[1]
        step = step.split('echo "== libproc darwin bindings (stable copy + plant helper) =="', 1)[0]
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            cargo_home = root / "cargo-home"
            cargo_home.mkdir()
            if cached_source:
                kernel = cargo_home / "registry/src/index.crates.io-test/lance-linalg-0.1/src/simd/dist_table.c"
                kernel.parent.mkdir(parents=True)
                kernel.write_text("/* fixture */\n")
            zig = root / "zig/zig"
            zig.parent.mkdir()
            zig.write_text(f"#!/bin/sh\nprintf 'compiler-invoked\\n'\nexit {compiler_exit}\n")
            zig.chmod(0o755)
            env = {**os.environ, "CARGO_HOME": str(cargo_home)}
            return subprocess.run(
                ["bash", "-c", "set -euo pipefail\n" + step.replace("/mnt/cargo", temp)],
                env=env, capture_output=True, text=True,
            )

    def test_empty_registry_skips_optional_kernel(self):
        result = self.run_kernel_step()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("lance-linalg source not in registry yet", result.stdout)
        self.assertNotIn("compiler-invoked", result.stdout)

    def test_cached_source_compiles_kernel(self):
        result = self.run_kernel_step(cached_source=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("compiler-invoked", result.stdout)
        self.assertIn("kernel compiled from", result.stdout)

    def test_compiler_failure_is_not_ignored(self):
        result = self.run_kernel_step(cached_source=True, compiler_exit=17)
        self.assertEqual(result.returncode, 17)
        self.assertNotIn("kernel compiled from", result.stdout)


class PublishedBundleTests(unittest.TestCase):
    def test_bundle_contains_assets_after_ebs_links_are_restored(self):
        self.check_bundle(ebs_links=True)

    def test_bundle_contains_assets_after_fresh_provisioning(self):
        self.check_bundle(ebs_links=False)

    def check_bundle(self, ebs_links):
        source = SCRIPT.read_text()
        publish = source.split('if [[ $PUBLISH_BUNDLE -eq 1 ]]; then', 1)[1].split('\nfi', 1)[0]
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            cache, durable = root / "cache", root / "cross"
            assets = durable if ebs_links else cache
            for folder in [cache / "tmp", cache / "cargo-home/bin", assets / "macsdk", assets / "zig-dist/zig-test"]:
                folder.mkdir(parents=True)
            (assets / "macsdk/osx_libproc_bindings.aarch64.rs").write_text("bindings")
            (assets / "zig-dist/zig-test/zig").write_text("#!/bin/sh\necho zig-ready\n")
            (assets / "zig-dist/zig-test/zig").chmod(0o755)
            (assets / "zig").symlink_to("zig-dist/zig-test")
            if ebs_links:
                for asset in ("macsdk", "zig-dist", "zig"):
                    (cache / asset).symlink_to(durable / asset)
            for tool in ("cargo-zigbuild", "bindgen"):
                (cache / "cargo-home/bin" / tool).write_text(tool)
            output = root / "bundle.tar.gz"
            prelude = """set -euo pipefail
log() { :; }
readlink() { python3 -c 'import os,sys; print(os.path.realpath(sys.argv[-1]))' "$@"; }
aws() { cp "$5" "$TEST_BUNDLE"; }
export -f aws readlink
remote_ssh() { bash -c "${1#--command=}"; }
"""
            result = subprocess.run(["bash", "-c", prelude + publish.replace("/mnt/cargo", str(cache))],
                env={**os.environ, "SCCACHE_BUCKET": "fixture", "SCCACHE_S3_REGION": "fixture",
                     "TEST_BUNDLE": str(output)}, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            with tarfile.open(output) as bundle:
                self.assertEqual(bundle.extractfile("macsdk/osx_libproc_bindings.aarch64.rs").read(), b"bindings")
                zig_link = bundle.getmember("zig")
                self.assertTrue(zig_link.issym())
                self.assertEqual(zig_link.linkname, "zig-dist/zig-test")
                extracted = root / "restored"
                bundle.extractall(extracted, filter="data")
                self.assertEqual((extracted / "zig/zig").read_text(), "#!/bin/sh\necho zig-ready\n")
                self.assertEqual(bundle.extractfile("zig-dist/zig-test/zig").read(), b"#!/bin/sh\necho zig-ready\n")
            # A replacement VM must be able to persist this bundle and lose its
            # temporary disk without losing the Zig executable.
            persistent = root / "restored-ebs"
            block = source.split("# BEGIN persist macOS cross assets\n", 1)[1].split("# END persist macOS cross assets", 1)[0]
            block = block.replace("/mnt/cargo", str(extracted)).replace("/opt/spur-cross", str(persistent))
            result = subprocess.run(["bash", "-c", prelude + 'sudo() { "$@"; }\n' + block],
                capture_output=True, text=True, env={**os.environ, "SPUR_AWS_EBS_TOOL_ROOT": "1"})
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            shutil.rmtree(extracted)
            self.assertEqual(subprocess.check_output([str(persistent / "zig/zig")], text=True).strip(), "zig-ready")


if __name__ == "__main__":
    unittest.main()
