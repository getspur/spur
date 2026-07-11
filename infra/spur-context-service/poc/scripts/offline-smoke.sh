#!/bin/sh
# Run the complete no-credential/no-AWS POC verification suite.
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
poc_root=$(CDPATH= cd -- "$script_dir/.." && pwd)
repo_root=$(CDPATH= cd -- "$poc_root/../../.." && pwd)

cd "$repo_root"

python3 -m unittest infra/spur-context-service/poc/tests/test_harness_static.py -v
python3 infra/spur-context-service/poc/scripts/verify-evidence.py
python3 infra/spur-context-service/poc/scripts/scan-secrets.py \
  infra/spur-context-service/poc/fixtures/sanitized-plan-log.json

terraform -chdir="$poc_root" fmt -check -recursive
terraform -chdir="$poc_root" init -backend=false -input=false
terraform -chdir="$poc_root" validate
terraform -chdir="$poc_root" test

terraform -chdir="$repo_root/infra/spur-context-service" fmt -check -recursive
terraform -chdir="$repo_root/infra/spur-context-service" init -backend=false -input=false
terraform -chdir="$repo_root/infra/spur-context-service" validate
terraform -chdir="$repo_root/infra/spur-context-service" test -test-directory=tests

for script in infra/spur-context-service/poc/scripts/*.sh; do
  sh -n "$script"
done
infra/spur-context-service/poc/scripts/verify-teardown.sh \
  fixture-empty \
  infra/spur-context-service/poc/fixtures/empty-inventory.json
infra/spur-context-service/poc/scripts/compare-production.sh \
  infra/spur-context-service/poc/fixtures/sanitized-plan-log.json \
  infra/spur-context-service/poc/fixtures/sanitized-plan-log.json \
  infra/spur-context-service/poc/fixtures/empty-inventory.json \
  infra/spur-context-service/poc/fixtures/empty-inventory.json

scripts/spur-cargo --dir infra/spur-context-service/poc/auth-client test
scripts/spur-cargo --dir crates/spur-context-service test --lib auth::tests
scripts/spur-cargo --dir crates/spur-context-service test --features lambda --lib \
  fixture_
scripts/spur-cargo --dir crates/spur-context-service test --test jobs_test \
  namespaced_auth_identities_remain_distinct_backlog_owners
scripts/spur-cargo --dir crates/spur-context-service test --features lambda --test mcp_test \
  namespaced_cross_owner_dedupe_preserves_owner_and_hides_status

echo "offline POC smoke complete: no AWS apply, credentials, or network smoke used"
