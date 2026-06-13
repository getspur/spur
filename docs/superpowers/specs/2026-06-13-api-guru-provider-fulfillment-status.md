# APIs.guru Provider Fulfillment Status

| Field | Value |
|---|---|
| Status | Current after Task 7 promotions |
| Date | 2026-06-13 |
| Scope | APIs.guru-backed Nango providers in the REST table gateway catalog |
| Source artifacts | `connections/experimental_manifest_index.json`, `connections/supported/*.connection.toml`, `list_nango_providers_tiers` assertions |

## Coverage

The bundled crosswalk covers every APIs.guru-backed provider currently exposed
through the Nango catalog path.

| Measure | Current |
|---|---:|
| APIs.guru-backed providers visible in Wizard/backend | 87/87 |
| APIs.guru spec rows traceable to provider/spec provenance | 295/295 |
| Total Nango providers still listed by backend | 851 |

Provider-level status is computed the same way as `list_nango_providers`:
committed curated manifests are `Ready`; remaining APIs.guru-backed providers
are `Candidate` when their crosswalk row has a usable base URL and supported
auth mode; otherwise they are `Blocked`.

| Status | Providers | Spec rows |
|---|---:|---:|
| Ready | 15 | 99 |
| Candidate | 52 | 170 |
| Blocked | 20 | 26 |

Blocked providers are still visible. The current blocked reasons are
`unsupported_auth` or `missing_base_url`; `trello` remains blocked on
`unsupported_auth`.

## Newly Ready Providers

Task 6 and Task 7 promoted ten providers to committed supported manifests. No
additional providers are promoted by this report.

| Provider key | Supported manifest | Tables | Actions | Notes |
|---|---|---:|---:|---|
| `github-pat` | `github.connection.toml` | 2 | 0 | Reuses the reviewed GitHub manifest alias. |
| `1password-events` | `1password_events.connection.toml` | 1 | 0 | Simple/API-key promotion batch. |
| `atlassian-admin` | `atlassian_admin.connection.toml` | 1 | 0 | Simple/API-key promotion batch. |
| `azure-devops` | `azure_devops.connection.toml` | 1 | 0 | Simple/basic-auth promotion batch. |
| `clicksend` | `clicksend.connection.toml` | 1 | 0 | Simple/basic-auth promotion batch. |
| `asana` | `asana.connection.toml` | 1 | 0 | Visible OAuth-family BYO-token batch. |
| `slack` | `slack.connection.toml` | 1 | 0 | Visible OAuth-family BYO-token batch. |
| `jira` | `jira.connection.toml` | 1 | 0 | Visible OAuth-family BYO-token batch. |
| `notion` | `notion.connection.toml` | 1 | 0 | Visible OAuth-family BYO-token batch. |
| `autotask` | `autotask.connection.toml` | 1 | 0 | Promoted in place of blocked `trello`. |

Together, the ten newly Ready providers expose 11 reviewed tables and 0 actions.
They account for 30 Ready APIs.guru spec rows because `github-pat` and `slack`
each have multiple crosswalk rows.

## Refresh Workflow

Regenerate the catalog from pinned local inputs, then inspect the fulfillment
matrix before changing docs:

```sh
scripts/spur-cargo run -p spur-rest-table-gateway --bin nango-catalog -- \
  resources/nango/packages/providers/providers.yaml \
  .spur/vendor/apis-guru/list.json \
  .spur/nango-catalog \
  --nango-commit 988efd014 \
  --apis-guru-fetched-at 2026-06-12T00:00:00Z \
  --experimental-crosswalk-manifests
```

Primary outputs:

- `.spur/nango-catalog/api_guru_fulfillment_matrix.json`
- `.spur/nango-catalog/coverage_summary.json`
- `.spur/nango-catalog/connections/experimental/*.connection.toml`

Only committed files under `connections/supported/` count as Ready. Generated
experimental manifests remain Candidate evidence until reviewed, committed, and
covered by provider-specific E2E tests.
