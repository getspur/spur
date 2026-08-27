//! Immutable Parquet-backed MCP code surface.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};
use spur_graph::{GraphQueryClient, ParquetClient, SearchFilters, SearchMode, SearchOptions};
use thiserror::Error;

use crate::artifact_cache::{
    ArtifactBundleIdentity, ArtifactCache, ArtifactCacheError, ArtifactIdentity,
    MaterializedArtifactBundle,
};
use crate::serving_registry::{ServingPackage, ServingRegistry};

#[derive(Debug, Clone)]
pub struct CatalogRequest {
    pub source: String,
    pub package: Option<String>,
    pub revision_or_ref: Option<String>,
    pub path: Option<String>,
    pub name_filter: Option<String>,
    pub limit: usize,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CodeSearchRequest {
    pub source: String,
    pub package: String,
    pub revision_or_ref: Option<String>,
    pub query: String,
    pub symbol_kind: Option<String>,
    pub limit: usize,
}

#[derive(Debug, Error)]
pub enum CodeBackendError {
    #[error("invalid serving registry")]
    InvalidRegistry,
    #[error("package unavailable")]
    PackageUnavailable,
    #[error("invalid catalog cursor")]
    InvalidCursor,
    #[error("{0}")]
    Artifact(#[from] ArtifactCacheError),
    #[error("artifact open failed")]
    ArtifactOpen,
    #[error("artifact query failed")]
    ArtifactQuery,
}

impl CodeBackendError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidRegistry => "invalid_serving_registry",
            Self::PackageUnavailable => "package_unavailable",
            Self::InvalidCursor => "invalid_catalog_cursor",
            Self::Artifact(error) => error.code(),
            Self::ArtifactOpen => "artifact_open_failed",
            Self::ArtifactQuery => "artifact_query_failed",
        }
    }

    pub fn is_retryable(&self) -> bool {
        match self {
            Self::PackageUnavailable | Self::InvalidCursor => false,
            Self::Artifact(error) => error.is_retryable(),
            Self::InvalidRegistry | Self::ArtifactOpen | Self::ArtifactQuery => true,
        }
    }
}

#[derive(Clone)]
pub struct CodeBackend {
    registry: ServingRegistry,
    cache: ArtifactCache,
}

struct OpenedPackage {
    package: ServingPackage,
    client: ParquetClient,
    _bundle: MaterializedArtifactBundle,
}

impl CodeBackend {
    pub fn new(registry: ServingRegistry, cache: ArtifactCache) -> Result<Self, CodeBackendError> {
        registry
            .validate()
            .map_err(|_| CodeBackendError::InvalidRegistry)?;
        Ok(Self { registry, cache })
    }

    pub fn generation(&self) -> i64 {
        self.registry.generation
    }

    pub async fn catalog(&self, request: CatalogRequest) -> Result<Value, CodeBackendError> {
        let Some(package) = request.package.as_deref() else {
            return self.catalog_packages(&request).await;
        };

        let Some(revision_or_ref) = request.revision_or_ref.as_deref() else {
            return self
                .catalog_revisions(&request.source, package, &request)
                .await;
        };

        let opened = self
            .open_selected(&request.source, package, Some(revision_or_ref))
            .await?;
        if let Some(path) = request.path.as_deref() {
            let is_file = opened
                .client
                .file_exists(path)
                .map_err(|_| CodeBackendError::ArtifactQuery)?;
            if is_file {
                return catalog_symbols(&opened, path, &request);
            }
        }
        catalog_tree(&opened, &request)
    }

    pub async fn search(&self, request: CodeSearchRequest) -> Result<Value, CodeBackendError> {
        let opened = self
            .open_selected(
                &request.source,
                &request.package,
                request.revision_or_ref.as_deref(),
            )
            .await?;
        let result = opened
            .client
            .search_symbols(&SearchOptions {
                query: request.query,
                mode: SearchMode::Substring,
                filters: SearchFilters {
                    symbol_kind: request.symbol_kind,
                    file: None,
                    file_glob: None,
                },
                limit: request.limit,
            })
            .map_err(|_| CodeBackendError::ArtifactQuery)?;
        let candidates = result
            .candidates
            .into_iter()
            .map(|symbol| {
                let selector_name = if symbol.qualified_name.is_empty() {
                    symbol.entity_name.as_str()
                } else {
                    symbol.qualified_name.as_str()
                };
                let selector = format!(
                    "pkg:{}@{}::{selector_name}",
                    opened.package.package, opened.package.revision
                );
                let uri = package_symbol_uri(
                    &opened.package.source,
                    &opened.package.package,
                    &opened.package.revision,
                    &symbol.stable_symbol_id,
                );
                json!({
                    "selector": selector,
                    "uri": uri,
                    "id": symbol.stable_symbol_id,
                    "stable_symbol_id": symbol.stable_symbol_id,
                    "source": opened.package.source,
                    "package": opened.package.package,
                    "revision": opened.package.revision,
                    "entity_name": symbol.entity_name,
                    "qualified_name": symbol.qualified_name,
                    "file_path": symbol.file_path,
                    "line_range": symbol.line_range,
                    "symbol_kind": symbol.symbol_kind,
                    "enclosing_scope": symbol.enclosing_scope,
                })
            })
            .collect::<Vec<_>>();
        Ok(json!({
            "candidates": candidates,
            "total_matches": result.total_matches,
            "truncated": result.truncated,
        }))
    }

    pub async fn warm_index(
        &self,
        source: &str,
        package: &str,
        revision: &str,
    ) -> Result<Option<Value>, CodeBackendError> {
        let selected = match self.resolve_exact(source, package, revision)? {
            Some(selected) => selected,
            None => return Ok(None),
        };
        let opened = self.open(selected).await?;
        Ok(Some(json!({
            "status": "complete",
            "snapshot_id": opened.package.generation,
            "revision": opened.package.revision,
        })))
    }

    pub async fn index_status(&self, job_id: &str) -> Result<Value, CodeBackendError> {
        let Some((package, revision)) = parse_warm_job_id(job_id) else {
            return Ok(json!({ "status": "not_found" }));
        };
        let selected = match self.resolve_exact("registry:crates-io", package, revision)? {
            Some(selected) => selected,
            None => return Ok(json!({ "status": "not_found" })),
        };
        let opened = self.open(selected).await?;
        Ok(json!({
            "job_id": job_id,
            "status": "complete",
            "revision": opened.package.revision,
            "created_at": "",
            "updated_at": "",
            "attempt": 0,
            "execution_arn": null,
            "snapshot_id": opened.package.generation,
            "row_counts": opened.client.manifest().row_counts,
        }))
    }

    async fn catalog_packages(&self, request: &CatalogRequest) -> Result<Value, CodeBackendError> {
        let mut revisions_by_package = BTreeMap::<String, Vec<String>>::new();
        for package in self.registry.packages.iter().filter(|candidate| {
            candidate.source == request.source
                && request
                    .name_filter
                    .as_deref()
                    .is_none_or(|filter| candidate.package.contains(filter))
        }) {
            let opened = self.open(package.clone()).await?;
            revisions_by_package
                .entry(package.package.clone())
                .or_default()
                .push(opened.package.revision.clone());
        }

        let cursor = decode_cursor(request.cursor.as_deref(), 2)?;
        let total_matches = revisions_by_package.len();
        let mut rows = revisions_by_package
            .into_iter()
            .filter(|(package, _)| {
                cursor.as_ref().is_none_or(|parts| {
                    (request.source.as_str(), package.as_str())
                        > (parts[0].as_str(), parts[1].as_str())
                })
            })
            .map(|(package, revisions)| {
                let latest_revision = revisions.iter().max().cloned();
                json!({
                    "source": request.source,
                    "package": package,
                    "latest_revision": latest_revision,
                    "revision_count": revisions.len(),
                    "indexed_at": "",
                })
            })
            .collect::<Vec<_>>();
        let (truncated, next_cursor) = trim_page(&mut rows, request.limit, |row| {
            encode_cursor(&[
                row["source"].as_str().unwrap_or_default(),
                row["package"].as_str().unwrap_or_default(),
            ])
        });
        Ok(catalog_page(
            "packages",
            rows,
            total_matches,
            truncated,
            next_cursor,
            self.registry.generation,
        ))
    }

    async fn catalog_revisions(
        &self,
        source: &str,
        package: &str,
        request: &CatalogRequest,
    ) -> Result<Value, CodeBackendError> {
        let mut rows = Vec::new();
        for selected in self
            .registry
            .packages
            .iter()
            .filter(|candidate| candidate.source == source && candidate.package == package)
        {
            let opened = self.open(selected.clone()).await?;
            let semver = normalized_semver(&opened.package.revision);
            rows.push(json!({
                "revision": opened.package.revision,
                "revision_kind": if semver.is_some() { "semver" } else { "opaque" },
                "semver": semver,
                "indexed_at": "",
                "embeddings_status": "skipped",
                "row_counts": opened.client.manifest().row_counts,
                "generation": opened.package.generation,
                "snapshot_id": opened.package.generation,
                "refs": [],
            }));
        }
        if rows.is_empty() {
            return Err(CodeBackendError::PackageUnavailable);
        }
        rows.sort_by(|left, right| left["revision"].as_str().cmp(&right["revision"].as_str()));
        let cursor = decode_cursor(request.cursor.as_deref(), 1)?;
        let total_matches = rows.len();
        rows.retain(|item| {
            cursor.as_ref().is_none_or(|parts| {
                item["revision"]
                    .as_str()
                    .is_some_and(|revision| revision > parts[0].as_str())
            })
        });
        let (truncated, next_cursor) = trim_page(&mut rows, request.limit, |row| {
            encode_cursor(&[row["revision"].as_str().unwrap_or_default()])
        });
        Ok(catalog_page(
            "revisions",
            rows,
            total_matches,
            truncated,
            next_cursor,
            self.registry.generation,
        ))
    }

    async fn open_selected(
        &self,
        source: &str,
        package: &str,
        revision_or_ref: Option<&str>,
    ) -> Result<OpenedPackage, CodeBackendError> {
        let selected = if let Some(revision) = revision_or_ref.filter(|value| *value != "latest") {
            self.resolve_exact(source, package, revision)?
        } else {
            self.registry
                .packages
                .iter()
                .filter(|candidate| candidate.source == source && candidate.package == package)
                .max_by(|left, right| left.revision.cmp(&right.revision))
                .cloned()
        }
        .ok_or(CodeBackendError::PackageUnavailable)?;
        self.open(selected).await
    }

    fn resolve_exact(
        &self,
        source: &str,
        package: &str,
        revision: &str,
    ) -> Result<Option<ServingPackage>, CodeBackendError> {
        self.registry
            .resolve(source, package, revision)
            .map(|package| package.cloned())
            .map_err(|_| CodeBackendError::InvalidRegistry)
    }

    async fn open(&self, package: ServingPackage) -> Result<OpenedPackage, CodeBackendError> {
        self.cache
            .activate_generation(self.registry.generation)
            .await?;
        let identity = ArtifactBundleIdentity {
            root: ArtifactIdentity {
                generation: package.generation,
                source: package.source.clone(),
                package: package.package.clone(),
                revision: package.revision.clone(),
                artifact: package.graph_manifest.clone(),
            },
            graph_prefix: package.graph_prefix_uri.clone(),
        };
        let bundle = self.cache.materialize_bundle(&identity).await?;
        let client =
            ParquetClient::open(bundle.path()).map_err(|_| CodeBackendError::ArtifactOpen)?;
        Ok(OpenedPackage {
            package,
            client,
            _bundle: bundle,
        })
    }
}

fn catalog_symbols(
    opened: &OpenedPackage,
    path: &str,
    request: &CatalogRequest,
) -> Result<Value, CodeBackendError> {
    let mut symbols = opened
        .client
        .symbols_by_file(path)
        .map_err(|_| CodeBackendError::ArtifactQuery)?;
    symbols.retain(|symbol| {
        request.name_filter.as_deref().is_none_or(|filter| {
            symbol.entity_name.contains(filter) || symbol.qualified_name.contains(filter)
        })
    });
    symbols.sort_by(|left, right| {
        left.line_range[0]
            .cmp(&right.line_range[0])
            .then_with(|| left.stable_symbol_id.cmp(&right.stable_symbol_id))
    });
    let total_matches = symbols.len();
    let cursor = decode_cursor(request.cursor.as_deref(), 2)?;
    let mut rows = symbols
        .into_iter()
        .filter(|symbol| {
            cursor.as_ref().is_none_or(|parts| {
                let line = parts[0].parse::<usize>().unwrap_or_default();
                (symbol.line_range[0], symbol.stable_symbol_id.as_str()) > (line, parts[1].as_str())
            })
        })
        .map(|symbol| {
            let name = if symbol.qualified_name.is_empty() {
                symbol.entity_name.as_str()
            } else {
                symbol.qualified_name.as_str()
            };
            let selector = format!(
                "pkg:{}@{}::{name}",
                opened.package.package, opened.package.revision
            );
            let next = [
                "external_code_read",
                "external_code_callers",
                "external_code_callees",
            ]
            .into_iter()
            .map(|tool| json!({ "tool": tool, "selector": selector }))
            .collect::<Vec<_>>();
            json!({
                "entity_name": symbol.entity_name,
                "qualified_name": symbol.qualified_name,
                "symbol_kind": symbol.symbol_kind,
                "line_range": symbol.line_range,
                "selector": selector,
                "next": next,
                "_cursor_id": symbol.stable_symbol_id,
            })
        })
        .collect::<Vec<_>>();
    let (truncated, next_cursor) = trim_page(&mut rows, request.limit, |row| {
        encode_cursor(&[
            &row["line_range"][0]
                .as_u64()
                .unwrap_or_default()
                .to_string(),
            row["_cursor_id"].as_str().unwrap_or_default(),
        ])
    });
    for row in &mut rows {
        row.as_object_mut()
            .map(|object| object.remove("_cursor_id"));
    }
    Ok(catalog_page(
        "symbols",
        rows,
        total_matches,
        truncated,
        next_cursor,
        opened.package.generation,
    ))
}

fn catalog_tree(
    opened: &OpenedPackage,
    request: &CatalogRequest,
) -> Result<Value, CodeBackendError> {
    let files = opened
        .client
        .files()
        .map_err(|_| CodeBackendError::ArtifactQuery)?;
    let paths = files
        .iter()
        .map(|file| file.file_path.clone())
        .collect::<Vec<_>>();
    let symbols = opened
        .client
        .symbols_by_files(&paths)
        .map_err(|_| CodeBackendError::ArtifactQuery)?;
    let symbol_counts = symbols
        .into_iter()
        .fold(BTreeMap::new(), |mut counts, symbol| {
            *counts.entry(symbol.file_path).or_insert(0_usize) += 1;
            counts
        });
    let prefix = request.path.as_deref().unwrap_or("").trim_matches('/');
    let mut entries = BTreeMap::<String, (String, bool, BTreeSet<String>, usize)>::new();
    for file in files {
        let remainder = if prefix.is_empty() {
            file.file_path.as_str()
        } else if let Some(remainder) = file
            .file_path
            .strip_prefix(prefix)
            .and_then(|value| value.strip_prefix('/'))
        {
            remainder
        } else {
            continue;
        };
        let Some((name, tail)) = remainder
            .split_once('/')
            .map(|(name, tail)| (name, Some(tail)))
            .or_else(|| (!remainder.is_empty()).then_some((remainder, None)))
        else {
            continue;
        };
        let path = if prefix.is_empty() {
            name.to_owned()
        } else {
            format!("{prefix}/{name}")
        };
        let entry = entries
            .entry(name.to_owned())
            .or_insert_with(|| (path, tail.is_some(), BTreeSet::new(), 0));
        entry.1 |= tail.is_some();
        entry.2.insert(file.file_path.clone());
        entry.3 += symbol_counts
            .get(&file.file_path)
            .copied()
            .unwrap_or_default();
    }
    let cursor = decode_cursor(request.cursor.as_deref(), 1)?;
    let total_matches = entries.len();
    let mut rows = entries
        .into_iter()
        .filter(|(name, _)| cursor.as_ref().is_none_or(|parts| name > &parts[0]))
        .map(|(name, (path, directory, files, symbol_count))| {
            json!({
                "name": name,
                "path": path,
                "kind": if directory { "dir" } else { "file" },
                "file_count": files.len(),
                "symbol_count": symbol_count,
            })
        })
        .collect::<Vec<_>>();
    let (truncated, next_cursor) = trim_page(&mut rows, request.limit, |row| {
        encode_cursor(&[row["name"].as_str().unwrap_or_default()])
    });
    Ok(catalog_page(
        "tree",
        rows,
        total_matches,
        truncated,
        next_cursor,
        opened.package.generation,
    ))
}

fn catalog_page(
    level: &str,
    rows: Vec<Value>,
    total_matches: usize,
    truncated: bool,
    next_cursor: Option<String>,
    generation: i64,
) -> Value {
    json!({
        "level": level,
        "rows": rows,
        "total_matches": total_matches,
        "truncated": truncated,
        "next_cursor": next_cursor,
        "catalog_generation": generation,
    })
}

fn trim_page(
    rows: &mut Vec<Value>,
    limit: usize,
    cursor_for: impl FnOnce(&Value) -> String,
) -> (bool, Option<String>) {
    let limit = limit.clamp(1, 200);
    let truncated = rows.len() > limit;
    if truncated {
        rows.truncate(limit);
    }
    let next_cursor = truncated.then(|| rows.last().map(cursor_for)).flatten();
    (truncated, next_cursor)
}

fn encode_uri_component(value: &str) -> String {
    value.replace('%', "%25").replace('/', "%2F")
}

fn decode_uri_component(value: &str) -> String {
    value.replace("%2F", "/").replace("%25", "%")
}

fn encode_cursor(parts: &[&str]) -> String {
    parts
        .iter()
        .map(|part| encode_uri_component(part))
        .collect::<Vec<_>>()
        .join("/")
}

fn decode_cursor(
    cursor: Option<&str>,
    expected_parts: usize,
) -> Result<Option<Vec<String>>, CodeBackendError> {
    let Some(cursor) = cursor else {
        return Ok(None);
    };
    let parts = cursor
        .split('/')
        .map(decode_uri_component)
        .collect::<Vec<_>>();
    if parts.len() != expected_parts || parts.iter().any(String::is_empty) {
        return Err(CodeBackendError::InvalidCursor);
    }
    Ok(Some(parts))
}

fn package_symbol_uri(source: &str, package: &str, revision: &str, stable_id: &str) -> String {
    format!(
        "pkg-symbol://{}/{}/{}/{}",
        encode_uri_component(source),
        encode_uri_component(package),
        encode_uri_component(revision),
        encode_uri_component(stable_id),
    )
}

fn normalized_semver(revision: &str) -> Option<String> {
    let revision = revision.strip_prefix('v').unwrap_or(revision);
    let mut parts = revision.split('.');
    let major = parts.next()?.parse::<u64>().ok()?;
    let minor = parts.next()?.parse::<u64>().ok()?;
    let patch = parts.next()?.parse::<u64>().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some(format!("{major}.{minor}.{patch}"))
}

fn parse_warm_job_id(job_id: &str) -> Option<(&str, &str)> {
    let value = job_id.strip_prefix("pkg:")?;
    let (package, revision) = value.split_once('@')?;
    (!package.is_empty() && !revision.is_empty() && !revision.contains("::"))
        .then_some((package, revision))
}
