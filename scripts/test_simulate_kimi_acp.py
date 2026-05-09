#!/usr/bin/env python3
import tempfile
import unittest
import uuid
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import simulate_kimi_acp as probe


class KimiAcpProbeTests(unittest.TestCase):
    def test_request_ids_match_sdk_uuid_string_shape(self):
        request_id = probe.new_request_id()

        self.assertIsInstance(request_id, str)
        uuid.UUID(request_id)

    def test_make_request_preserves_string_id(self):
        request_id = str(uuid.uuid4())

        msg = probe.make_request(request_id, "initialize", {})

        self.assertEqual(msg["id"], request_id)

    def test_slice_text_matches_native_line_limit_semantics(self):
        text = "one\ntwo\nthree\n"

        self.assertEqual(probe.slice_text(text, line=2, limit=1), "two")
        self.assertEqual(probe.slice_text(text, line=2, limit=None), "two\nthree")
        self.assertEqual(probe.slice_text(text, line=None, limit=2), "one\ntwo")

    def test_probe_context_handles_relative_file_read(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "sample.txt").write_text("alpha\nbeta\ngamma\n")
            ctx = probe.ProbeContext(root, allow_writes=False)

            result = ctx.handle_client_request(
                {
                    "method": "fs/read_text_file",
                    "params": {"path": "sample.txt", "line": 2, "limit": 1},
                }
            )

        self.assertEqual(result, {"content": "beta"})


if __name__ == "__main__":
    unittest.main()
