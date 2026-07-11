# Codex Explicit-Agent Artifact Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship deterministic Codex primary-profile activation plus native child-role availability with a restricted live POC whose local artifacts and exact new Codex rollouts are private, symlink-safe, and auditable.

**Architecture:** Keep the validated startup-profile and native-role implementation from the recovery base. Harden only the Python probe boundary: create a fresh private output root, use no-follow file/directory primitives, set `umask 0077` in the restricted child process, audit exact new rollouts before reading them, and fail closed. Re-run the positive and negative live controls, update the July 10 addendum, then execute the full Rust workspace gate remotely.

**Tech Stack:** Python 3 standard library (`os`, `stat`, `pathlib`, `unittest`), Rust workspace tests through `scripts/spur-cargo`, ACP adapter `@agentclientprotocol/codex-acp@1.1.2`, Codex CLI `0.144.1`, Ruff, Git.

---

## Scope and file map

- Modify `scripts/probe_acp_subagents.py`: private filesystem primitives, restricted-process umask, exact rollout audit, consistent ACP tool-title normalization, and verdict integration.
- Modify `scripts/test_probe_acp_subagents.py`: RED security, rollout, umask, and normalization regressions.
- Modify `docs/rca/2026-07-10-codex-0.144.1-profile-reprobe.md`: replace the invalid complete-tree claim with separately scoped local-tree and exact-rollout evidence from the hardened live run.
- Verify without redesigning:
  - `crates/spur-acp/src/profile_strategy.rs`
  - `crates/spur-acp/src/connection/native.rs`
  - `crates/spur-core/src/orchestrator/connection.rs`
  - `crates/spur-core/src/orchestrator/delegation/worker_attempt.rs`
  - `crates/spur-core/src/agent_profiles/render.rs`
- Do not modify `docs/rca/2026-07-05-codex-model-effort-profile-subagent-evaluation.md`.
- Do not modify an existing user-owned `.codex/agents/*.toml` or `~/.codex/config.toml`.

## SPUR execution mapping

Submit three sequential SPUR tasks, all assigned to `agent=codex`, `profile=rust-engineer`, `model=gpt-5.6-sol`, and `effort=max`:

1. `codex-explicit-agent-artifact-hardening`: Tasks 1-3 below; lineage reference `bd-26am2`.
2. `codex-explicit-agent-live-poc`: Task 4; depends on task 1 and retains the hardened overlay.
3. `codex-explicit-agent-final-gate`: Task 5; depends on task 2; lineage reference `bd-3rpmj`.

### Task 1: Add RED artifact, rollout, umask, and normalization regressions

**Files:**
- Modify: `scripts/test_probe_acp_subagents.py:230-303`
- Modify: `scripts/test_probe_acp_subagents.py:602-619`
- Modify: `scripts/test_probe_acp_subagents.py:832-936`
- Test: `scripts/test_probe_acp_subagents.py`

- [ ] **Step 1: Add fresh-root, nested-directory, app-log, and final-tree regressions**

Add these test methods to `AcpSubagentProbeTests`. Replace `test_prepare_app_server_log_clears_stale_warning`; a hardened run requires a fresh log target and must reject an existing entry.

```python
    @unittest.skipUnless(os.name == "posix", "POSIX no-follow contract")
    def test_fresh_private_root_rejects_existing_and_symlink_entries(self):
        with tempfile.TemporaryDirectory() as tmp:
            parent = Path(tmp)
            victim = parent / "victim"
            victim.mkdir()
            link = parent / "linked-output"
            link.symlink_to(victim, target_is_directory=True)

            with self.assertRaises(OSError):
                probe.create_fresh_private_root(link)
            self.assertTrue(victim.is_dir())

            existing = parent / "existing-output"
            existing.mkdir()
            with self.assertRaises(FileExistsError):
                probe.create_fresh_private_root(existing)

    @unittest.skipUnless(os.name == "posix", "POSIX no-follow contract")
    def test_private_directory_rejects_nested_symlink(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = probe.create_fresh_private_root(Path(tmp) / "artifacts")
            victim = Path(tmp) / "victim-directory"
            victim.mkdir()
            link = root / "positive-workspace"
            link.symlink_to(victim, target_is_directory=True)

            with self.assertRaises(OSError):
                probe.ensure_private_directory(link, exist_ok=False)
            self.assertEqual(list(victim.iterdir()), [])

    @unittest.skipUnless(os.name == "posix", "POSIX no-follow contract")
    def test_prepare_app_server_log_rejects_symlinked_directory_and_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = probe.create_fresh_private_root(Path(tmp) / "artifacts")

            victim_dir = Path(tmp) / "external-log-directory"
            victim_dir.mkdir()
            victim_log = victim_dir / "app-server.log"
            victim_log.write_text("external-content")
            linked_dir = root / "positive-app-server-logs"
            linked_dir.symlink_to(victim_dir, target_is_directory=True)
            with self.assertRaises(OSError):
                probe.prepare_app_server_log(linked_dir)
            self.assertEqual(victim_log.read_text(), "external-content")

            safe_dir = probe.ensure_private_directory(
                root / "negative-app-server-logs", exist_ok=False
            )
            second_victim = Path(tmp) / "external-file"
            second_victim.write_text("do-not-replace")
            (safe_dir / "app-server.log").symlink_to(second_victim)
            with self.assertRaises(OSError):
                probe.prepare_app_server_log(safe_dir, directory_exists=True)
            self.assertEqual(second_victim.read_text(), "do-not-replace")

    @unittest.skipUnless(os.name == "posix", "POSIX no-follow contract")
    def test_prepare_app_server_log_rejects_existing_regular_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = probe.create_fresh_private_root(Path(tmp) / "artifacts")
            log_dir = probe.ensure_private_directory(
                root / "positive-app-server-logs", exist_ok=False
            )
            log_path = log_dir / "app-server.log"
            log_path.write_text(probe.MALFORMED_ROLE_WARNING)

            with self.assertRaises(FileExistsError):
                probe.prepare_app_server_log(log_dir, directory_exists=True)
            self.assertEqual(log_path.read_text(), probe.MALFORMED_ROLE_WARNING)

    @unittest.skipUnless(os.name == "posix", "POSIX no-follow contract")
    def test_final_artifact_audit_rejects_symlinks(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = probe.create_fresh_private_root(Path(tmp) / "artifacts")
            victim = Path(tmp) / "victim"
            victim.write_text("outside")
            (root / "linked-artifact").symlink_to(victim)

            with self.assertRaises(OSError):
                probe.protect_artifact_tree(root)
            self.assertEqual(victim.read_text(), "outside")
```

- [ ] **Step 2: Add restricted-process umask and rollout audit regressions**

Add these methods to the same test class:

```python
    @unittest.skipUnless(os.name == "posix", "POSIX mode contract")
    def test_restricted_profile_process_sets_private_umask(self):
        with tempfile.TemporaryDirectory() as tmp:
            output = Path(tmp) / "restricted-output"
            code = "\n".join(
                [
                    "from pathlib import Path",
                    "import probe_acp_subagents as probe",
                    "probe.configure_restricted_profile_process(True)",
                    f"root = Path({str(output)!r})",
                    "root.mkdir()",
                    "(root / 'rollout.jsonl').write_text('private')",
                ]
            )
            completed = subprocess.run(
                [sys.executable, "-c", code],
                cwd=Path(probe.__file__).resolve().parent,
                capture_output=True,
                text=True,
                check=False,
            )

            self.assertEqual(completed.returncode, 0, completed.stderr)
            self.assertEqual(output.stat().st_mode & 0o777, 0o700)
            self.assertEqual(
                (output / "rollout.jsonl").stat().st_mode & 0o777,
                0o600,
            )

    @unittest.skipUnless(os.name == "posix", "POSIX mode contract")
    def test_rollout_audit_hardens_exact_file_and_directory_chain(self):
        with tempfile.TemporaryDirectory() as tmp:
            session_root = Path(tmp) / "sessions"
            rollout_dir = session_root / "2026" / "07" / "10"
            rollout_dir.mkdir(parents=True, mode=0o755)
            for directory in (
                session_root,
                session_root / "2026",
                session_root / "2026" / "07",
                rollout_dir,
            ):
                directory.chmod(0o755)
            rollout = rollout_dir / "rollout-test.jsonl"
            rollout.write_text('{"type":"session_meta"}\n')
            rollout.chmod(0o644)

            audit = probe.audit_codex_rollouts(session_root, {rollout})

            self.assertEqual(audit.failures, ())
            self.assertEqual(audit.paths, (rollout,))
            self.assertEqual(rollout.stat().st_mode & 0o777, 0o600)
            for directory in (
                session_root,
                session_root / "2026",
                session_root / "2026" / "07",
                rollout_dir,
            ):
                self.assertEqual(directory.stat().st_mode & 0o777, 0o700)

    @unittest.skipUnless(os.name == "posix", "POSIX no-follow contract")
    def test_rollout_audit_rejects_symlink_missing_and_out_of_root_paths(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            session_root = root / "sessions"
            session_root.mkdir()
            victim = root / "victim.jsonl"
            victim.write_text("external")
            linked = session_root / "rollout-linked.jsonl"
            linked.symlink_to(victim)
            missing = session_root / "rollout-missing.jsonl"

            audit = probe.audit_codex_rollouts(
                session_root,
                {linked, missing, victim},
            )

            self.assertEqual(audit.paths, ())
            self.assertTrue(any("symlink" in item for item in audit.failures))
            self.assertTrue(any("missing" in item for item in audit.failures))
            self.assertTrue(any("outside" in item for item in audit.failures))
            self.assertEqual(victim.read_text(), "external")
```

- [ ] **Step 3: Add the space-separated ACP spawn-title regression**

Add this method beside the existing `load_codex_probe_evidence` tests:

```python
    def test_space_separated_spawn_title_participates_in_canary_scan(self):
        records = [
            {
                "dir": "recv",
                "msg": {
                    "method": "session/update",
                    "params": {
                        "update": {
                            "sessionUpdate": "tool_call",
                            "toolCallId": "spawn-1",
                            "title": "spawn agent",
                            "rawInput": {"message": "PRIMARY-123"},
                        }
                    },
                },
            }
        ]
        with tempfile.TemporaryDirectory() as tmp:
            raw_log = Path(tmp) / "positive.jsonl"
            raw_log.write_text("\n".join(json.dumps(item) for item in records))
            evidence = probe.load_codex_probe_evidence(raw_log)

        self.assertEqual(evidence.spawn_agent_call_ids, ("spawn-1",))
        self.assertEqual(
            evidence.spawn_agent_raw_inputs,
            (json.dumps({"message": "PRIMARY-123"}, sort_keys=True),),
        )
```

- [ ] **Step 4: Run the focused suite and capture the RED result**

Run:

```bash
python3 -m unittest scripts/test_probe_acp_subagents.py
```

Expected: FAIL. The new tests must expose missing `create_fresh_private_root`, `configure_restricted_profile_process`, and `audit_codex_rollouts`; the app-server and final-tree tests must expose the current symlink-following/skip behavior; the space-separated title test must show no captured spawn input.

- [ ] **Step 5: Commit the failing regressions**

```bash
git add scripts/test_probe_acp_subagents.py
git commit -m "test(probe): bd-26am2 cover private Codex artifacts"
```

### Task 2: Implement fresh no-follow local artifacts and restricted-process umask

**Files:**
- Modify: `scripts/probe_acp_subagents.py:16-32`
- Modify: `scripts/probe_acp_subagents.py:173-226`
- Modify: `scripts/probe_acp_subagents.py:459-466`
- Modify: `scripts/probe_acp_subagents.py:1284-1350`
- Modify: `scripts/probe_acp_subagents.py:1474-1510`
- Modify: `scripts/probe_acp_subagents.py:2020-2101`
- Test: `scripts/test_probe_acp_subagents.py`

- [ ] **Step 1: Replace the private filesystem helpers**

Add `import stat` with the other standard-library imports. Replace `ensure_private_directory`, `write_private_text`, `open_private_text`, `open_private_descriptor`, and `protect_artifact_tree`; add the helper functions below.

The `dir_fd` calls are part of the security contract: every probe-owned child entry is
created or opened relative to an already verified no-follow parent descriptor. Do not
replace them with path-based `mkdir`, `touch`, `unlink`, or truncate operations.

```python
def require_posix_private_filesystem() -> None:
    required = ("O_DIRECTORY", "O_NOFOLLOW")
    if os.name != "posix" or any(not hasattr(os, name) for name in required):
        raise OSError("restricted profile probe requires POSIX no-follow mode support")


def _open_directory_no_follow(path: Path) -> int:
    require_posix_private_filesystem()
    return os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)


def _harden_directory(path: Path) -> None:
    descriptor = _open_directory_no_follow(path)
    try:
        before = os.fstat(descriptor)
        if not stat.S_ISDIR(before.st_mode):
            raise OSError(f"private artifact path is not a directory: {path}")
        os.fchmod(descriptor, 0o700)
        if os.fstat(descriptor).st_mode & 0o777 != 0o700:
            raise OSError(f"private artifact directory mode is not 0700: {path}")
    finally:
        os.close(descriptor)


def _harden_regular_file(path: Path) -> None:
    require_posix_private_filesystem()
    parent_descriptor = _open_directory_no_follow(path.parent)
    try:
        descriptor = os.open(
            path.name,
            os.O_RDONLY | os.O_NOFOLLOW,
            dir_fd=parent_descriptor,
        )
        try:
            before = os.fstat(descriptor)
            if not stat.S_ISREG(before.st_mode):
                raise OSError(f"private artifact path is not a regular file: {path}")
            os.fchmod(descriptor, 0o600)
            if os.fstat(descriptor).st_mode & 0o777 != 0o600:
                raise OSError(f"private artifact file mode is not 0600: {path}")
        finally:
            os.close(descriptor)
    finally:
        os.close(parent_descriptor)


def ensure_private_directory(path: Path, *, exist_ok: bool = True) -> Path:
    require_posix_private_filesystem()
    parent_descriptor = _open_directory_no_follow(path.parent)
    try:
        try:
            current = os.stat(path.name, dir_fd=parent_descriptor, follow_symlinks=False)
        except FileNotFoundError:
            os.mkdir(path.name, 0o700, dir_fd=parent_descriptor)
        else:
            if stat.S_ISLNK(current.st_mode):
                raise OSError(f"private artifact directory cannot be a symlink: {path}")
            if not stat.S_ISDIR(current.st_mode):
                raise OSError(f"private artifact path is not a directory: {path}")
            if not exist_ok:
                raise FileExistsError(f"private artifact directory already exists: {path}")
        descriptor = os.open(
            path.name,
            os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
            dir_fd=parent_descriptor,
        )
        try:
            os.fchmod(descriptor, 0o700)
            if os.fstat(descriptor).st_mode & 0o777 != 0o700:
                raise OSError(f"private artifact directory mode is not 0700: {path}")
        finally:
            os.close(descriptor)
    finally:
        os.close(parent_descriptor)
    return path


def create_fresh_private_root(requested: Path) -> Path:
    require_posix_private_filesystem()
    requested = requested.expanduser()
    try:
        existing = os.lstat(requested)
    except FileNotFoundError:
        existing = None
    if existing is not None:
        if stat.S_ISLNK(existing.st_mode):
            raise OSError(f"profile probe output root cannot be a symlink: {requested}")
        raise FileExistsError(f"profile probe output root already exists: {requested}")
    requested.parent.mkdir(parents=True, exist_ok=True)
    parent = requested.parent.resolve(strict=True)
    root = parent / requested.name
    parent_descriptor = _open_directory_no_follow(parent)
    try:
        os.mkdir(root.name, 0o700, dir_fd=parent_descriptor)
        descriptor = os.open(
            root.name,
            os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
            dir_fd=parent_descriptor,
        )
        try:
            os.fchmod(descriptor, 0o700)
            if os.fstat(descriptor).st_mode & 0o777 != 0o700:
                raise OSError(f"profile probe output root mode is not 0700: {root}")
        finally:
            os.close(descriptor)
    finally:
        os.close(parent_descriptor)
    return root


def open_private_descriptor(path: Path) -> int:
    require_posix_private_filesystem()
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW
    parent_descriptor = _open_directory_no_follow(path.parent)
    try:
        descriptor = os.open(
            path.name,
            flags,
            0o600,
            dir_fd=parent_descriptor,
        )
    finally:
        os.close(parent_descriptor)
    try:
        current = os.fstat(descriptor)
        if not stat.S_ISREG(current.st_mode):
            raise OSError(f"private artifact path is not a regular file: {path}")
        os.fchmod(descriptor, 0o600)
        if os.fstat(descriptor).st_mode & 0o777 != 0o600:
            raise OSError(f"private artifact file mode is not 0600: {path}")
    except BaseException:
        os.close(descriptor)
        raise
    return descriptor


def write_private_text(path: Path, text: str) -> None:
    descriptor = open_private_descriptor(path)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8") as stream:
            descriptor = -1
            stream.write(text)
    finally:
        if descriptor >= 0:
            os.close(descriptor)


def open_private_text(path: Path) -> Any:
    return os.fdopen(open_private_descriptor(path), "w", encoding="utf-8")


def read_text_no_follow(path: Path) -> str:
    require_posix_private_filesystem()
    parent_descriptor = _open_directory_no_follow(path.parent)
    try:
        descriptor = os.open(
            path.name,
            os.O_RDONLY | os.O_NOFOLLOW,
            dir_fd=parent_descriptor,
        )
    finally:
        os.close(parent_descriptor)
    try:
        if not stat.S_ISREG(os.fstat(descriptor).st_mode):
            raise OSError(f"private evidence path is not a regular file: {path}")
        with os.fdopen(descriptor, "r", encoding="utf-8", errors="replace") as stream:
            descriptor = -1
            return stream.read()
    finally:
        if descriptor >= 0:
            os.close(descriptor)


def protect_artifact_tree(root: Path) -> None:
    try:
        root_state = os.lstat(root)
    except FileNotFoundError:
        return
    if stat.S_ISLNK(root_state.st_mode) or not stat.S_ISDIR(root_state.st_mode):
        raise OSError(f"artifact root is not a no-follow directory: {root}")
    _harden_directory(root)
    for current_root, directories, files in os.walk(root, followlinks=False):
        current = Path(current_root)
        for name in sorted(directories):
            path = current / name
            state = os.lstat(path)
            if stat.S_ISLNK(state.st_mode):
                raise OSError(f"artifact tree contains a directory symlink: {path}")
            if not stat.S_ISDIR(state.st_mode):
                raise OSError(f"artifact tree contains an unsupported entry: {path}")
            _harden_directory(path)
        for name in sorted(files):
            path = current / name
            state = os.lstat(path)
            if stat.S_ISLNK(state.st_mode):
                raise OSError(f"artifact tree contains a file symlink: {path}")
            if not stat.S_ISREG(state.st_mode):
                raise OSError(f"artifact tree contains an unsupported entry: {path}")
            _harden_regular_file(path)
```

- [ ] **Step 2: Make app-server log preparation fresh and no-follow**

Replace `prepare_app_server_log` with:

```python
def prepare_app_server_log(
    log_dir: Path,
    *,
    directory_exists: bool = False,
) -> Path:
    if directory_exists:
        ensure_private_directory(log_dir, exist_ok=True)
    else:
        ensure_private_directory(log_dir, exist_ok=False)
    log_path = log_dir / "app-server.log"
    descriptor = open_private_descriptor(log_path)
    os.close(descriptor)
    return log_path
```

The production call sites use the default `directory_exists=False`. The explicit flag exists only for the regression that plants a final-file symlink in an already verified test directory.

Replace `read_text_if_present` with a no-follow read:

```python
def read_text_if_present(path: Path) -> Optional[str]:
    try:
        return read_text_no_follow(path)
    except FileNotFoundError:
        return None
```

- [ ] **Step 3: Set the private umask only in restricted inner processes**

Add:

```python
def configure_restricted_profile_process(restricted: bool) -> None:
    if not restricted:
        return
    require_posix_private_filesystem()
    os.umask(0o077)
```

Immediately after `args = parser.parse_args()` in `main`, add:

```python
    try:
        configure_restricted_profile_process(args.restricted_profile_probe)
    except OSError as exc:
        print(f"[BLOCKED] restricted artifact invariant unsupported: {exc}", file=sys.stderr)
        return 2
```

Do not set the umask in the outer `--codex-profile-probe` process. `codex_profile_inner_command` already adds `--restricted-profile-probe`, so the adapter, Codex app-server, and native children inherit `0077` only in the restricted controls.

- [ ] **Step 4: Use the shared title normalizer in evidence loading**

In `load_codex_probe_evidence`, replace:

```python
        title = str(snapshot.get("title") or "").replace("_", "").lower()
```

with:

```python
        title = normalized_acp_tool_title(str(snapshot.get("title") or ""))
```

Also replace the raw-log read loop header with:

```python
    for raw_line in read_text_no_follow(raw_log_path).splitlines():
```

- [ ] **Step 5: Create a fresh output leaf before resolving it**

In `run_codex_profile_probe`, replace `base_out = base_out.resolve()` plus `ensure_private_directory(base_out)` with:

```python
    try:
        base_out = create_fresh_private_root(base_out)
    except OSError as exc:
        print(f"[FAIL] unsafe profile probe output root: {exc}", file=sys.stderr)
        return 1
```

Every subsequent directory is created one level at a time beneath the verified root. Keep `initialize_codex_probe_workspace` and `create_codex_profile_fixture` calls in their existing order; their directory calls now reject symlinks and unsupported entries instead of following them.

- [ ] **Step 6: Make every early return enforce the local-tree audit**

Add:

```python
def finalize_profile_artifacts(root: Path, requested_exit: int) -> int:
    try:
        protect_artifact_tree(root)
    except OSError as exc:
        print(f"[FAIL] local artifact audit failed: {exc}", file=sys.stderr)
        return 1
    return requested_exit
```

Replace every `protect_artifact_tree(base_out); return 1` pair in
`run_codex_profile_probe` with:

```python
        return finalize_profile_artifacts(base_out, 1)
```

Replace every `protect_artifact_tree(base_out); return 2` pair with:

```python
        return finalize_profile_artifacts(base_out, 2)
```

Replace the final block with:

```python
    print(f"artifacts={base_out}")
    if failures:
        for failure in failures:
            print(f"[FAIL] {failure}", file=sys.stderr)
        return finalize_profile_artifacts(base_out, 1)
    audited_exit = finalize_profile_artifacts(base_out, 0)
    if audited_exit != 0:
        return audited_exit
    print(f"PROFILE_PROBE_PASS label={label}")
    return 0
```

- [ ] **Step 7: Run the local-artifact regressions**

Run:

```bash
python3 -m unittest scripts.test_probe_acp_subagents.AcpSubagentProbeTests.test_fresh_private_root_rejects_existing_and_symlink_entries
python3 -m unittest scripts.test_probe_acp_subagents.AcpSubagentProbeTests.test_private_directory_rejects_nested_symlink
python3 -m unittest scripts.test_probe_acp_subagents.AcpSubagentProbeTests.test_prepare_app_server_log_rejects_symlinked_directory_and_file
python3 -m unittest scripts.test_probe_acp_subagents.AcpSubagentProbeTests.test_prepare_app_server_log_rejects_existing_regular_file
python3 -m unittest scripts.test_probe_acp_subagents.AcpSubagentProbeTests.test_final_artifact_audit_rejects_symlinks
python3 -m unittest scripts.test_probe_acp_subagents.AcpSubagentProbeTests.test_restricted_profile_process_sets_private_umask
python3 -m unittest scripts.test_probe_acp_subagents.AcpSubagentProbeTests.test_space_separated_spawn_title_participates_in_canary_scan
```

Expected: each command reports `OK`.

### Task 3: Audit exact new Codex rollouts before evidence readers consume them

**Files:**
- Modify: `scripts/probe_acp_subagents.py:129-145`
- Modify: `scripts/probe_acp_subagents.py:499-581`
- Modify: `scripts/probe_acp_subagents.py:584-678`
- Modify: `scripts/probe_acp_subagents.py:1556-1639`
- Modify: `scripts/probe_acp_subagents.py:1681-1727`
- Test: `scripts/test_probe_acp_subagents.py`

- [ ] **Step 1: Add the rollout audit result type and implementation**

Add beside `CodexRolloutActivity`:

```python
@dataclass(frozen=True)
class CodexRolloutAudit:
    paths: tuple[Path, ...]
    directory_count: int
    failures: tuple[str, ...]
```

Add after `codex_rollout_paths`:

```python
def _rollout_directory_chain(session_root: Path, parent: Path) -> tuple[Path, ...]:
    chain = []
    current = parent
    while True:
        chain.append(current)
        if current == session_root:
            return tuple(reversed(chain))
        if current.parent == current:
            raise OSError(f"rollout path escaped expected session root: {parent}")
        current = current.parent


def audit_codex_rollouts(
    session_root: Path,
    rollout_paths: set[Path] | list[Path] | tuple[Path, ...],
) -> CodexRolloutAudit:
    failures = []
    safe_paths = []
    directories = set()
    try:
        canonical_root = session_root.expanduser().resolve(strict=True)
    except OSError as exc:
        return CodexRolloutAudit(
            paths=(),
            directory_count=0,
            failures=(f"session root unavailable: {type(exc).__name__}: {exc}",),
        )
    if not rollout_paths:
        return CodexRolloutAudit(
            paths=(),
            directory_count=0,
            failures=("no new Codex rollout files were discovered",),
        )

    for supplied in sorted(rollout_paths):
        path = Path(os.path.abspath(supplied))
        try:
            path.relative_to(canonical_root)
        except ValueError:
            failures.append(f"rollout path is outside expected session root: {path}")
            continue
        try:
            state = os.lstat(path)
        except FileNotFoundError:
            failures.append(f"rollout path is missing: {path}")
            continue
        except OSError as exc:
            failures.append(f"rollout lstat failed for {path}: {type(exc).__name__}: {exc}")
            continue
        if stat.S_ISLNK(state.st_mode):
            failures.append(f"rollout path is a symlink: {path}")
            continue
        if not stat.S_ISREG(state.st_mode):
            failures.append(f"rollout path is not a regular file: {path}")
            continue
        try:
            for directory in _rollout_directory_chain(canonical_root, path.parent):
                directory_state = os.lstat(directory)
                if stat.S_ISLNK(directory_state.st_mode):
                    raise OSError(f"rollout directory is a symlink: {directory}")
                if not stat.S_ISDIR(directory_state.st_mode):
                    raise OSError(f"rollout parent is not a directory: {directory}")
                if directory_state.st_uid != os.getuid():
                    raise OSError(f"rollout directory is not owned by current user: {directory}")
                _harden_directory(directory)
                directories.add(directory)
            _harden_regular_file(path)
        except OSError as exc:
            failures.append(str(exc))
            continue
        safe_paths.append(path)

    if failures:
        safe_paths = []
    return CodexRolloutAudit(
        paths=tuple(safe_paths),
        directory_count=len(directories),
        failures=tuple(failures),
    )
```

- [ ] **Step 2: Make rollout readers no-follow**

In both `load_codex_rollout_activity` and `load_codex_role_binding`, replace `path.read_text(...).splitlines()` with:

```python
read_text_no_follow(path).splitlines()
```

Keep the existing activity-reader `try/except` behavior. Add the same `try/except OSError` fail-closed evidence error to `load_codex_role_binding` so a post-audit read failure becomes `CodexRoleBinding.evidence_error` instead of an uncaught exception.

Use this exact read block at the top of the role-binding loop:

```python
        try:
            raw_lines = read_text_no_follow(path).splitlines()
        except OSError as exc:
            return CodexRoleBinding(
                parent_thread_id=None,
                requested_agent_type=None,
                spawn_call_id=None,
                child_thread_id=None,
                child_parent_thread_id=None,
                child_agent_role=None,
                evidence_error=(
                    f"{path}: read failed: {type(exc).__name__}: {exc}"
                ),
            )
        for raw_line in raw_lines:
```

- [ ] **Step 3: Audit positive and negative rollout sets before parsing**

Immediately after calculating each before/after set in `run_codex_profile_probe`, add:

```python
    positive_rollout_audit = audit_codex_rollouts(session_root, positive_rollouts)
    if positive_rollout_audit.failures:
        for failure in positive_rollout_audit.failures:
            print(f"[FAIL] positive rollout audit: {failure}", file=sys.stderr)
        print(f"artifacts={base_out}")
        return finalize_profile_artifacts(base_out, 1)
    positive_rollouts = set(positive_rollout_audit.paths)
```

and:

```python
    negative_rollout_audit = audit_codex_rollouts(session_root, negative_rollouts)
    if negative_rollout_audit.failures:
        for failure in negative_rollout_audit.failures:
            print(f"[FAIL] negative rollout audit: {failure}", file=sys.stderr)
        print(f"artifacts={base_out}")
        return finalize_profile_artifacts(base_out, 1)
    negative_rollouts = set(negative_rollout_audit.paths)
```

Only audited paths may be passed to `load_codex_role_binding` or `load_codex_rollout_activity`.

- [ ] **Step 4: Print sanitized audit evidence and gate success on both scopes**

Before the final `artifacts=` line, add:

```python
    print(
        "rollout_private_audit="
        f"{str(not positive_rollout_audit.failures and not negative_rollout_audit.failures).lower()}"
    )
    print(
        "rollout_audit_counts="
        f"positive_files:{len(positive_rollout_audit.paths)},"
        f"negative_files:{len(negative_rollout_audit.paths)},"
        f"directories:{positive_rollout_audit.directory_count + negative_rollout_audit.directory_count}"
    )
```

Do not print rollout contents, canaries, auth values, or complete `CODEX_CONFIG`.

- [ ] **Step 5: Run the complete probe unit gate**

Run:

```bash
python3 -m unittest scripts/test_probe_acp_subagents.py
python3 -m py_compile scripts/probe_acp_subagents.py scripts/test_probe_acp_subagents.py
node --check scripts/probe-codex-acp.mjs
ruff check scripts/probe_acp_subagents.py scripts/test_probe_acp_subagents.py
ruff format --check scripts/probe_acp_subagents.py scripts/test_probe_acp_subagents.py
git diff --check
```

Expected: all commands exit zero; unittest reports exactly 57 passing tests and
contains every regression from Task 1.

- [ ] **Step 6: Commit the minimal GREEN implementation**

```bash
git add scripts/probe_acp_subagents.py scripts/test_probe_acp_subagents.py
git commit -m "fix(probe): bd-26am2 harden Codex profile artifacts"
```

### Task 4: Run the hardened live POC and correct the July 10 evidence record

**Files:**
- Modify: `docs/rca/2026-07-10-codex-0.144.1-profile-reprobe.md:11-46`
- Modify: `docs/rca/2026-07-10-codex-0.144.1-profile-reprobe.md:48-244`
- Verify: `scripts/probe_acp_subagents.py`
- Verify: `scripts/test_probe_acp_subagents.py`

- [ ] **Step 1: Run the focused hardened live POC**

Run from the delegated task worktree reported by `pwd`, allowing the probe to choose
its fresh default output leaf. Capture the sanitized console output in a private
temporary file so the artifact path can be reused for the independent mode check:

```bash
LIVE_OUTPUT="$(mktemp -t spur-codex-profile-poc.XXXXXX)"
python3 scripts/probe_acp_subagents.py \
  --agent codex \
  --codex-profile-probe \
  --codex-package @agentclientprotocol/codex-acp@1.1.2 \
  --timeout 180 \
  --init-timeout 45 \
  --session-timeout 180 >"$LIVE_OUTPUT" 2>&1
LIVE_STATUS="$?"
sed -n '/^adapter_version=/p;/^codex_cli_version=/p;/^primary_token_in_/p;/^child_token_in_/p;/^exact_child_role_/p;/^no_profile_negative_control=/p;/^restricted_client_capabilities=/p;/^unexpected_profile_activity=/p;/^rollout_private_audit=/p;/^artifacts=/p;/^PROFILE_PROBE_PASS/p' "$LIVE_OUTPUT"
test "$LIVE_STATUS" -eq 0
```

Expected successful evidence:

```text
adapter_version=1.1.2
codex_cli_version=0.144.1
primary_token_in_primary_response=true
child_token_in_raw_spawn_agent_result=true
exact_child_role_requested=true
exact_child_role_loaded=true
no_profile_negative_control=true
restricted_client_capabilities=true
unexpected_profile_activity=false
rollout_private_audit=true
PROFILE_PROBE_PASS label=codex-0.144.1
```

Authentication, network, npm resolution, or adapter-startup failure must return exit 2 and be recorded as blocked. A symlink, unsafe mode, out-of-root rollout, missing rollout, or incomplete audit must return exit 1 and must not be reclassified as blocked.

- [ ] **Step 2: Independently inspect modes without reading token contents**

Extract the printed `artifacts=` path, then run:

```bash
ARTIFACT_ROOT="$(sed -n 's/^artifacts=//p' "$LIVE_OUTPUT" | tail -1)"
test -n "$ARTIFACT_ROOT"
find "$ARTIFACT_ROOT" -type l -print
find "$ARTIFACT_ROOT" -type d -exec stat -f '%Sp %N' {} \;
find "$ARTIFACT_ROOT" -type f -exec stat -f '%Sp %N' {} \;
rm -f "$LIVE_OUTPUT"
```

Expected: the symlink command prints nothing; every directory begins `drwx------`; every regular file begins `-rw-------`. Do not print rollout contents or canary-bearing artifact contents.

- [ ] **Step 3: Update the July 10 addendum with separate evidence scopes**

Replace the statement that the local output directory is the complete token-bearing tree. Record these separately:

```markdown
- The probe-owned output tree passed a no-follow audit: every directory was `0700`,
  every regular file was `0600`, and no symlink or unsupported entry existed.
- The exact new Codex rollout files used for positive and negative evidence were
  beneath the canonical session root, regular no-follow files at `0600`, and every
  current-user-owned directory in their session-root chain was verified at `0700`.
- Restricted adapter and Codex descendants inherited `umask 0077`, preventing a
  group/world-readable creation window before the outer descriptor audit.
```

Append the fresh unit counts, live command, sanitized verdict fields, and the exact executed adapter/CLI versions. State explicitly that the prior functional result remained valid but the previous complete-tree privacy claim was superseded.

- [ ] **Step 4: Verify the documentation diff and commit live evidence**

```bash
git diff --check
git diff -- docs/rca/2026-07-10-codex-0.144.1-profile-reprobe.md
git add docs/rca/2026-07-10-codex-0.144.1-profile-reprobe.md
git commit -m "docs(rca): bd-26am2 record hardened Codex profile proof"
```

Expected: only the July 10 addendum changes; the July 5 RCA remains untouched.

### Task 5: Run the final remote integration and regression gate

**Files:**
- Verify: `crates/spur-acp/src/profile_strategy.rs`
- Verify: `crates/spur-acp/src/connection/native.rs`
- Verify: `crates/spur-core/src/orchestrator/connection.rs`
- Verify: `crates/spur-core/src/orchestrator/delegation/worker_attempt.rs`
- Verify: `crates/spur-core/src/agent_profiles/render.rs`
- Verify: `scripts/probe_acp_subagents.py`
- Verify: `scripts/test_probe_acp_subagents.py`
- Verify: `docs/rca/2026-07-10-codex-0.144.1-profile-reprobe.md`

- [ ] **Step 1: Confirm the final overlay and TDD provenance**

```bash
git status --short
git log --oneline --decorate -8
git diff --check
```

Expected: clean worktree; a RED test commit precedes the GREEN implementation commit; no unrelated file is modified.

- [ ] **Step 2: Run format and focused Python gates**

```bash
scripts/spur-cargo fmt --all -- --check
python3 -m unittest scripts/test_probe_acp_subagents.py
python3 -m py_compile scripts/probe_acp_subagents.py scripts/test_probe_acp_subagents.py
node --check scripts/probe-codex-acp.mjs
ruff check scripts/probe_acp_subagents.py scripts/test_probe_acp_subagents.py
ruff format --check scripts/probe_acp_subagents.py scripts/test_probe_acp_subagents.py
```

Expected: every command exits zero.

- [ ] **Step 3: Run the required Rust gates through remote `spur-cargo`**

```bash
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-acp
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-core
SPUR_REMOTE=1 scripts/spur-cargo clippy \
  -p spur-acp -p spur-core --all-targets -- -D warnings
SPUR_REMOTE=1 scripts/spur-cargo test --workspace
```

Expected: every command exits zero. A real remote test or clippy failure is not rerun locally to obtain a different result.

- [ ] **Step 4: Reconfirm the explicit-agent contract from emitted evidence**

Verify the approved contract, without requiring ordinary delegations to spawn a child:

```text
SPUR profile body -> CODEX_CONFIG.developer_instructions -> primary threadStart
.codex/agents/rust-engineer.toml -> spawn_agent(agent_type="rust-engineer") -> child agent_role
request model/effort -> ACP config options -> wins over profile defaults
```

Expected: primary activation is deterministic; the native child role is available and exact when requested; no process-global environment mutation, existing Codex config modification, or cross-attempt leakage occurs.

- [ ] **Step 5: Record the final verification state**

Final-gate work is verification-only. If verification changes no files, do not create
an empty commit. Report exact commands, pass counts, live evidence status, branch name,
and commit IDs in the delegation outcome. If a required correction is discovered,
stop the final-gate task and return the exact failing command and affected path to the
implementation task rather than making an opportunistic unreviewed edit.
