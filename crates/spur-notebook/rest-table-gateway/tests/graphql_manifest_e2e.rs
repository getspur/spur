use arrow_array::{BooleanArray, Float64Array, Int64Array, StringArray};
use arrow_schema::DataType;
use spur_rest_table_gateway::adapter::manifest::{AuthCfg, Manifest, Transport};
use spur_rest_table_gateway::adapter::manifest_adapter::ManifestAdapter;
use spur_rest_table_gateway::adapter::{
    ActionRequest, Adapter, Predicate, PredicateOp, ResolvedAuth, ScalarValue, ScanRequest,
};
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const ISSUES_QUERY: &str = "query Issues($owner: String!, $repoName: String!, $state: [String!]!) { repository(owner: $owner, name: $repoName) { issues(states: $state, first: 100) { nodes { id number title open weight } } } }";
const LINEAR_ISSUES_QUERY: &str = "query Issues($first:Int!, $after:String, $stateFilter:String){ issues(first:$first, after:$after, filter:{state:{name:{eq:$stateFilter}}}){ nodes{ id identifier title priority state{name} assignee{name} createdAt } pageInfo{ hasNextPage endCursor } } }";
const LINEAR_TEAMS_QUERY: &str = "query Teams{ teams(first:250){ nodes{ id key name description timezone cyclesEnabled createdAt updatedAt } pageInfo{ hasNextPage endCursor } } }";
const LINEAR_CREATE_ISSUE_MUTATION: &str = "mutation CreateIssue($title:String!, $teamId:String!, $description:String){ issueCreate(input:{title:$title, teamId:$teamId, description:$description}){ issue{ id identifier url } } }";

fn scan_request(table: &str) -> ScanRequest {
    ScanRequest {
        table: table.to_string(),
        predicates: vec![Predicate {
            column: "repository".to_string(),
            op: PredicateOp::Eq,
            value: ScalarValue::Utf8("spur".to_string()),
        }],
        projection: None,
        tvf_args: Vec::new(),
        auth: ResolvedAuth::None,
    }
}

#[tokio::test]
async fn graphql_manifest_action_posts_mutation_variables_and_returns_typed_row() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_json(serde_json::json!({
            "query": LINEAR_CREATE_ISSUE_MUTATION,
            "variables": {
                "title": "Ship GraphQL actions",
                "teamId": "team_123",
                "description": "Make Linear writes two-way"
            }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": {
                "issueCreate": {
                    "issue": {
                        "id": "lin_123",
                        "identifier": "ENG-123",
                        "url": "https://linear.app/acme/issue/ENG-123"
                    }
                }
            }
        })))
        .mount(&server)
        .await;

    let manifest = Manifest::from_toml(&format!(
        r#"
[source]
name = "linear"
base_url = "{}/graphql"
transport = "graphql"
allow_writes = true

[[action]]
name = "create_issue"
method = "POST"
path = "/graphql"
response_path = "$.data.issueCreate.issue"

[action.graphql]
query = "{}"
arg_vars = {{ title = "title", teamId = "teamId", description = "description" }}

[action.args]
title = {{ in = "body", type = "Utf8", required = true }}
teamId = {{ in = "body", type = "Utf8", required = true }}
description = {{ in = "body", type = "Utf8" }}

[action.columns]
id = {{ json = "$.id", type = "Utf8" }}
identifier = {{ json = "$.identifier", type = "Utf8" }}
url = {{ json = "$.url", type = "Utf8" }}
"#,
        server.uri(),
        LINEAR_CREATE_ISSUE_MUTATION
    ))
    .expect("manifest toml should parse");
    let adapter = ManifestAdapter::new(manifest);

    let batches = adapter
        .act(ActionRequest {
            name: "create_issue".to_string(),
            method: "POST".to_string(),
            path: "/graphql".to_string(),
            query: vec![],
            body: Some(serde_json::json!({
                "title": "Ship GraphQL actions",
                "teamId": "team_123",
                "description": "Make Linear writes two-way"
            })),
            idempotency_key: None,
            dry_run: false,
        })
        .await
        .expect("GraphQL action should return typed mutation result");

    assert_eq!(batches.len(), 1);
    let batch = &batches[0];
    assert_eq!(batch.num_rows(), 1);
    assert_eq!(batch.schema().field(0).data_type(), &DataType::Utf8);
    assert_eq!(batch.schema().field(1).data_type(), &DataType::Utf8);
    assert_eq!(batch.schema().field(2).data_type(), &DataType::Utf8);

    let ids = batch
        .column(0)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("id should be Utf8");
    let identifiers = batch
        .column(1)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("identifier should be Utf8");
    let urls = batch
        .column(2)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("url should be Utf8");

    assert_eq!(ids.value(0), "lin_123");
    assert_eq!(identifiers.value(0), "ENG-123");
    assert_eq!(urls.value(0), "https://linear.app/acme/issue/ENG-123");
}

#[tokio::test]
async fn graphql_manifest_scan_maps_eq_predicate_to_variable_and_builds_typed_batch() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_json(serde_json::json!({
            "query": ISSUES_QUERY,
            "variables": {
                "owner": "spur-org",
                "state": ["OPEN"],
                "repoName": "spur"
            }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": {
                "repository": {
                    "issues": {
                        "nodes": [
                            {
                                "id": "I_1",
                                "number": 42,
                                "title": "GraphQL pushdown",
                                "open": true,
                                "weight": "2.5"
                            }
                        ]
                    }
                }
            }
        })))
        .mount(&server)
        .await;

    let manifest = Manifest::from_toml(&format!(
        r#"
[source]
name = "github"
base_url = "{}/graphql"
transport = "graphql"

[[table]]
name = "issues"
path = "/unused"
response_path = "$.data.repository.issues.nodes"

[table.columns]
id = {{ json = "$.id", type = "Utf8" }}
number = {{ json = "$.number", type = "Int64" }}
title = {{ json = "$.title", type = "Utf8" }}
open = {{ json = "$.open", type = "Boolean" }}
weight = {{ json = "$.weight", type = "Float64" }}

[table.graphql]
query = "{}"
variables = {{ owner = "spur-org", state = ["OPEN"] }}

[table.graphql.arg_vars]
repository = "repoName"
"#,
        server.uri(),
        ISSUES_QUERY
    ))
    .expect("manifest toml should parse");
    let adapter = ManifestAdapter::new(manifest);

    let batches = adapter
        .scan(scan_request("issues"))
        .await
        .expect("scan should fetch typed GraphQL rows");

    assert_eq!(batches.len(), 1);
    let batch = &batches[0];
    assert_eq!(batch.num_rows(), 1);
    assert_eq!(batch.schema().field(0).data_type(), &DataType::Utf8);
    assert_eq!(batch.schema().field(1).data_type(), &DataType::Int64);
    assert_eq!(batch.schema().field(3).data_type(), &DataType::Boolean);
    assert_eq!(batch.schema().field(4).data_type(), &DataType::Float64);

    let ids = batch
        .column(0)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("id should be Utf8");
    let numbers = batch
        .column(1)
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("number should be Int64");
    let titles = batch
        .column(2)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("title should be Utf8");
    let open = batch
        .column(3)
        .as_any()
        .downcast_ref::<BooleanArray>()
        .expect("open should be Boolean");
    let weights = batch
        .column(4)
        .as_any()
        .downcast_ref::<Float64Array>()
        .expect("weight should be Float64");

    assert_eq!(ids.value(0), "I_1");
    assert_eq!(numbers.value(0), 42);
    assert_eq!(titles.value(0), "GraphQL pushdown");
    assert!(open.value(0));
    assert!((weights.value(0) - 2.5).abs() < f64::EPSILON);
}

#[tokio::test]
async fn linear_connection_manifest_parses_and_scans_graphql_issues() {
    let manifest_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("connections/supported/linear.connection.toml");
    let manifest_toml =
        std::fs::read_to_string(manifest_path).expect("linear manifest should exist");
    let mut manifest = Manifest::from_toml(&manifest_toml).expect("linear manifest should parse");

    assert_eq!(manifest.source.name, "linear");
    assert_eq!(manifest.source.base_url, "https://api.linear.app/graphql");
    assert_eq!(manifest.source.transport, Transport::Graphql);
    assert!(manifest.source.allow_writes);
    match &manifest.source.auth {
        AuthCfg::Header { name, env } => {
            assert_eq!(name, "authorization");
            assert_eq!(env, "LINEAR_API_KEY");
        }
        other => panic!("expected header auth, got {other:?}"),
    }

    let pagination = manifest.source.pagination.as_ref().expect("pagination");
    assert_eq!(pagination.style, "cursor");
    assert_eq!(pagination.page_size, 50);
    assert_eq!(pagination.cursor_param.as_deref(), Some("after"));
    assert_eq!(
        pagination.cursor_path.as_deref(),
        Some("$.data.issues.pageInfo.endCursor")
    );
    assert_eq!(
        pagination.has_next_path.as_deref(),
        Some("$.data.issues.pageInfo.hasNextPage")
    );

    let table = manifest
        .tables
        .iter()
        .find(|table| table.name == "issues")
        .expect("issues table");
    assert_eq!(table.response_path.as_deref(), Some("$.data.issues.nodes"));
    assert_eq!(table.filters["state"].param, "stateFilter");
    let graphql = table.graphql.as_ref().expect("graphql table config");
    assert_eq!(graphql.query, LINEAR_ISSUES_QUERY);
    assert_eq!(graphql.arg_vars["state"], "stateFilter");

    let expected_tables = [
        "issues",
        "projects",
        "teams",
        "users",
        "cycles",
        "issue_labels",
        "workflow_states",
        "comments",
        "organization",
    ];
    for expected in expected_tables {
        assert!(
            manifest.tables.iter().any(|table| table.name == expected),
            "missing Linear table {expected}"
        );
    }
    let teams_table = manifest
        .tables
        .iter()
        .find(|table| table.name == "teams")
        .expect("teams table");
    assert_eq!(
        teams_table.response_path.as_deref(),
        Some("$.data.teams.nodes")
    );
    let teams_graphql = teams_table.graphql.as_ref().expect("teams graphql config");
    assert_eq!(teams_graphql.query, LINEAR_TEAMS_QUERY);

    let create_issue = manifest
        .actions
        .iter()
        .find(|action| action.name == "create_issue")
        .expect("create_issue action");
    assert_eq!(
        create_issue.response_path.as_deref(),
        Some("$.data.issueCreate.issue")
    );
    let create_issue_graphql = create_issue.graphql.as_ref().expect("create_issue graphql");
    assert_eq!(create_issue_graphql.query, LINEAR_CREATE_ISSUE_MUTATION);
    assert_eq!(create_issue_graphql.arg_vars["title"], "title");
    assert_eq!(create_issue_graphql.arg_vars["teamId"], "teamId");
    assert_eq!(create_issue_graphql.arg_vars["description"], "description");
    for expected in [
        "create_issue",
        "update_issue",
        "create_comment",
        "create_project",
    ] {
        assert!(
            manifest
                .actions
                .iter()
                .any(|action| action.name == expected),
            "missing Linear action {expected}"
        );
    }

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_json(serde_json::json!({
            "query": LINEAR_ISSUES_QUERY,
            "variables": {
                "stateFilter": "In Progress",
                "first": 50
            }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": {
                "issues": {
                    "nodes": [
                        {
                            "id": "lin_1",
                            "identifier": "ENG-1",
                            "title": "Wire GraphQL manifest",
                            "priority": 1,
                            "state": { "name": "In Progress" },
                            "assignee": { "name": "Ada" },
                            "createdAt": "2026-06-01T00:00:00.000Z"
                        },
                        {
                            "id": "lin_2",
                            "identifier": "ENG-2",
                            "title": "Verify Linear rows",
                            "priority": 2,
                            "state": { "name": "In Progress" },
                            "assignee": { "name": "Grace" },
                            "createdAt": "2026-06-02T00:00:00.000Z"
                        }
                    ],
                    "pageInfo": {
                        "hasNextPage": false,
                        "endCursor": "cursor-2"
                    }
                }
            }
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_json(serde_json::json!({
            "query": LINEAR_TEAMS_QUERY,
            "variables": {
                "first": 50
            }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": {
                "teams": {
                    "nodes": [
                        {
                            "id": "team_1",
                            "key": "ENG",
                            "name": "Engineering",
                            "description": "Product engineering",
                            "timezone": "America/Los_Angeles",
                            "cyclesEnabled": true,
                            "createdAt": "2026-01-01T00:00:00.000Z",
                            "updatedAt": "2026-01-02T00:00:00.000Z"
                        }
                    ],
                    "pageInfo": {
                        "hasNextPage": true,
                        "endCursor": "ignored-by-global-issues-pagination"
                    }
                }
            }
        })))
        .mount(&server)
        .await;

    manifest.source.base_url = format!("{}/graphql", server.uri());
    let adapter = ManifestAdapter::new(manifest);
    let batches = adapter
        .scan(ScanRequest {
            table: "issues".to_string(),
            predicates: vec![Predicate {
                column: "state".to_string(),
                op: PredicateOp::Eq,
                value: ScalarValue::Utf8("In Progress".to_string()),
            }],
            projection: None,
            tvf_args: vec![],
            auth: ResolvedAuth::None,
        })
        .await
        .expect("scan should fetch typed Linear issues");

    assert_eq!(batches.len(), 1);
    let batch = &batches[0];
    assert_eq!(batch.num_rows(), 2);
    assert_eq!(batch.schema().field(0).data_type(), &DataType::Utf8);
    assert_eq!(batch.schema().field(3).data_type(), &DataType::Int64);
    assert_eq!(batch.schema().field(6).data_type(), &DataType::Utf8);

    let ids = batch
        .column(0)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("id should be Utf8");
    let identifiers = batch
        .column(1)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("identifier should be Utf8");
    let titles = batch
        .column(2)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("title should be Utf8");
    let priorities = batch
        .column(3)
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("priority should be Int64");
    let states = batch
        .column(4)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("state should be Utf8");
    let assignees = batch
        .column(5)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("assignee should be Utf8");
    let created_at = batch
        .column(6)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("created_at should be Utf8");

    assert_eq!(ids.value(0), "lin_1");
    assert_eq!(ids.value(1), "lin_2");
    assert_eq!(identifiers.value(0), "ENG-1");
    assert_eq!(titles.value(1), "Verify Linear rows");
    assert_eq!(priorities.value(0), 1);
    assert_eq!(priorities.value(1), 2);
    assert_eq!(states.value(0), "In Progress");
    assert_eq!(assignees.value(1), "Grace");
    assert_eq!(created_at.value(0), "2026-06-01T00:00:00.000Z");

    let team_batches = adapter
        .scan(ScanRequest {
            table: "teams".to_string(),
            predicates: vec![],
            projection: None,
            tvf_args: vec![],
            auth: ResolvedAuth::None,
        })
        .await
        .expect("scan should fetch typed Linear teams");

    assert_eq!(team_batches.len(), 1);
    let team_batch = &team_batches[0];
    assert_eq!(team_batch.num_rows(), 1);
    assert_eq!(team_batch.schema().field(0).data_type(), &DataType::Utf8);
    assert_eq!(team_batch.schema().field(5).data_type(), &DataType::Boolean);

    let team_keys = team_batch
        .column(1)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("team key should be Utf8");
    let team_names = team_batch
        .column(2)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("team name should be Utf8");
    let cycles_enabled = team_batch
        .column(5)
        .as_any()
        .downcast_ref::<BooleanArray>()
        .expect("cycles_enabled should be Boolean");

    assert_eq!(team_keys.value(0), "ENG");
    assert_eq!(team_names.value(0), "Engineering");
    assert!(cycles_enabled.value(0));
}
