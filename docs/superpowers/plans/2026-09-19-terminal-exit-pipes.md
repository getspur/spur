# Fix terminal exit reporting independently of inherited pipes

Issue: bd-3stsw. Contract: ../specs/2026-09-19-terminal-exit-pipes.md.

1. Enable and strengthen the committed native ACP held-pipe regression;
   add release/kill/shutdown coverage. Run remote and commit the failing tests.
2. Observe command exit concurrently, bound final output drain at 50 ms,
   retain readers for late output, and route cleanup to the task owning Child.
   Keep changes inside spur-acp.
3. Run focused native tests, then the full crate; check formatting/lints.
   Fix meaningful failures and request independent review before final commit.
4. Verify post-fix bounded observations with the workflow catalog, retain
   results, document limits, commit the fix, and close the issue with evidence.
