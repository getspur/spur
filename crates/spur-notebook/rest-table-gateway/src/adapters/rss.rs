use std::sync::Arc;

use arrow_array::{ArrayRef, BooleanArray, Int64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;

use crate::adapter::{Adapter, ScalarValue, ScanRequest, TableDef, TableKind};
use crate::error::{GatewayError, Result};

const DEFAULT_RSSHUB_BASE: &str = "https://rsshub.app";
const DEFAULT_RSSHUB_ROUTES_URL: &str = "https://docs.rsshub.app/routes.json";

const DEFAULT_RSSHUB_ROUTE_CARDS: &[DefaultRsshubRouteCard] = &[DefaultRsshubRouteCard {
    table: "hackernews_jobs_entries",
    source_url: "rsshub://hackernews/jobs",
    public_instance_fetch_url: Some("https://hnrss.org/jobs"),
}];

struct DefaultRsshubRouteCard {
    table: &'static str,
    source_url: &'static str,
    public_instance_fetch_url: Option<&'static str>,
}

pub struct RssAdapter {
    rsshub_base: String,
    routes_url: String,
    subscriptions: Vec<RssSubscription>,
    client: Client,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RssSubscription {
    table: String,
    url: String,
}

#[derive(Debug, Deserialize)]
struct RssSubscriptionConfig {
    table: Option<String>,
    name: Option<String>,
    url: Option<String>,
    feed_url: Option<String>,
}

#[derive(Debug, Default)]
struct ParsedFeed {
    title: Option<String>,
    description: Option<String>,
    site_url: Option<String>,
    entries: Vec<ParsedEntry>,
}

#[derive(Debug, Default)]
struct ParsedEntry {
    guid: Option<String>,
    title: Option<String>,
    url: Option<String>,
    description: Option<String>,
    published_at: Option<String>,
    author: Option<String>,
    categories: Vec<String>,
}

impl RssAdapter {
    pub fn new() -> Self {
        Self::with_rsshub_base(DEFAULT_RSSHUB_BASE)
    }

    pub fn with_rsshub_base(rsshub_base: &str) -> Self {
        Self::with_config(rsshub_base, DEFAULT_RSSHUB_ROUTES_URL)
    }

    pub fn with_config(rsshub_base: &str, routes_url: &str) -> Self {
        Self {
            rsshub_base: rsshub_base.trim_end_matches('/').to_string(),
            routes_url: routes_url.to_string(),
            subscriptions: Vec::new(),
            client: crate::adapter::default_http_client(),
        }
    }

    pub fn with_subscriptions(mut self, subscriptions: Vec<RssSubscription>) -> Self {
        self.subscriptions = subscriptions;
        self
    }

    fn subscription_feed_url(&self, table: &str) -> Option<String> {
        self.subscriptions
            .iter()
            .find(|subscription| subscription.table == table)
            .map(|subscription| subscription.url.clone())
            .or_else(|| default_route_card_url(table).map(str::to_string))
    }

    fn feed_schema() -> Arc<Schema> {
        Arc::new(Schema::new(vec![
            Field::new("url", DataType::Utf8, true),
            Field::new("title", DataType::Utf8, true),
            Field::new("description", DataType::Utf8, true),
            Field::new("site_url", DataType::Utf8, true),
        ]))
    }

    fn entries_schema() -> Arc<Schema> {
        Arc::new(Schema::new(vec![
            Field::new("feed_url", DataType::Utf8, true),
            Field::new("guid", DataType::Utf8, true),
            Field::new("title", DataType::Utf8, true),
            Field::new("url", DataType::Utf8, true),
            Field::new("description", DataType::Utf8, true),
            Field::new("published_at", DataType::Utf8, true),
            Field::new("author", DataType::Utf8, true),
            Field::new("categories", DataType::Utf8, true),
        ]))
    }

    fn routes_schema() -> Arc<Schema> {
        Arc::new(Schema::new(vec![
            Field::new("source_key", DataType::Utf8, true),
            Field::new("source_name", DataType::Utf8, true),
            Field::new("route", DataType::Utf8, true),
            Field::new("name", DataType::Utf8, true),
            Field::new("url", DataType::Utf8, true),
            Field::new("categories", DataType::Utf8, true),
            Field::new("heat", DataType::Int64, true),
            Field::new("example", DataType::Utf8, true),
            Field::new("rsshub_url", DataType::Utf8, true),
            Field::new("description", DataType::Utf8, true),
            Field::new("require_config", DataType::Boolean, true),
            Field::new("require_puppeteer", DataType::Boolean, true),
            Field::new("support_radar", DataType::Boolean, true),
            Field::new("top_feed_url", DataType::Utf8, true),
        ]))
    }

    fn feed_url_arg(args: &[ScalarValue], table: &str) -> Result<String> {
        match args.first() {
            Some(ScalarValue::Utf8(url)) => Ok(url.clone()),
            _ => Err(GatewayError::Adapter(format!(
                "rss_{table}(url): arg 0 must be a feed URL string"
            ))),
        }
    }

    fn resolved_fetch_url(&self, url: &str) -> Result<String> {
        if let Some(route) = url.strip_prefix("rsshub://") {
            let route = route.trim_start_matches('/');
            if route.is_empty() {
                return Err(GatewayError::Adapter(
                    "rsshub:// URL must include a route".to_string(),
                ));
            }
            if self.rsshub_base == DEFAULT_RSSHUB_BASE {
                if let Some(fetch_url) = public_instance_route_fallback(route) {
                    return Ok(fetch_url.to_string());
                }
            }
            return Ok(format!("{}/{}", self.rsshub_base, route));
        }
        if url.starts_with("http://") || url.starts_with("https://") {
            return Ok(url.to_string());
        }
        Err(GatewayError::Adapter(format!(
            "RSS feed URL must start with http://, https://, or rsshub://: {url}"
        )))
    }

    async fn fetch_feed(&self, original_url: &str) -> Result<String> {
        let fetch_url = self.resolved_fetch_url(original_url)?;
        self.fetch_text(&fetch_url, "RSS feed").await
    }

    async fn fetch_text(&self, url: &str, label: &str) -> Result<String> {
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|error| GatewayError::Http(error.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            return Err(GatewayError::Http(format!(
                "{label} request failed with HTTP {status}"
            )));
        }
        response
            .text()
            .await
            .map_err(|error| GatewayError::Http(error.to_string()))
    }

    async fn scan_feed(&self, url: String) -> Result<Vec<RecordBatch>> {
        let text = self.fetch_feed(&url).await?;
        let parsed = parse_feed(&text);
        record_batch(
            Self::feed_schema(),
            vec![
                vec![Some(url)],
                vec![parsed.title],
                vec![parsed.description],
                vec![parsed.site_url],
            ],
        )
    }

    async fn scan_entries(&self, url: String) -> Result<Vec<RecordBatch>> {
        let text = self.fetch_feed(&url).await?;
        let parsed = parse_feed(&text);
        let row_count = parsed.entries.len();
        let mut feed_urls = Vec::with_capacity(row_count);
        let mut guids = Vec::with_capacity(row_count);
        let mut titles = Vec::with_capacity(row_count);
        let mut urls = Vec::with_capacity(row_count);
        let mut descriptions = Vec::with_capacity(row_count);
        let mut published = Vec::with_capacity(row_count);
        let mut authors = Vec::with_capacity(row_count);
        let mut categories = Vec::with_capacity(row_count);

        for entry in parsed.entries {
            feed_urls.push(Some(url.clone()));
            guids.push(entry.guid);
            titles.push(entry.title);
            urls.push(entry.url);
            descriptions.push(entry.description);
            published.push(entry.published_at);
            authors.push(entry.author);
            categories.push(if entry.categories.is_empty() {
                None
            } else {
                Some(entry.categories.join(","))
            });
        }

        record_batch(
            Self::entries_schema(),
            vec![
                feed_urls,
                guids,
                titles,
                urls,
                descriptions,
                published,
                authors,
                categories,
            ],
        )
    }

    async fn scan_routes(&self) -> Result<Vec<RecordBatch>> {
        let text = self
            .fetch_text(&self.routes_url, "RSSHub routes catalog")
            .await?;
        let value: serde_json::Value = serde_json::from_str(&text)
            .map_err(|error| GatewayError::Adapter(error.to_string()))?;
        let Some(sources) = value.as_object() else {
            return Err(GatewayError::Adapter(
                "RSSHub routes catalog must be a JSON object".to_string(),
            ));
        };

        let mut source_keys = Vec::new();
        let mut source_names = Vec::new();
        let mut routes = Vec::new();
        let mut names = Vec::new();
        let mut urls = Vec::new();
        let mut categories = Vec::new();
        let mut heats = Vec::new();
        let mut examples = Vec::new();
        let mut rsshub_urls = Vec::new();
        let mut descriptions = Vec::new();
        let mut require_configs = Vec::new();
        let mut require_puppeteers = Vec::new();
        let mut support_radars = Vec::new();
        let mut top_feed_urls = Vec::new();

        for (source_key, source) in sources {
            let source_name = json_string(source, "name");
            let source_categories = json_string_array(source.get("categories"));
            let Some(route_map) = source.get("routes").and_then(|routes| routes.as_object()) else {
                continue;
            };

            for (route, route_meta) in route_map {
                let route_categories =
                    json_string_array(route_meta.get("categories")).or(source_categories.clone());
                let example = json_string(route_meta, "example");
                let rsshub_url = example
                    .as_deref()
                    .map(|example| format!("rsshub://{}", example.trim_start_matches('/')));
                let features = route_meta.get("features");

                source_keys.push(Some(source_key.clone()));
                source_names.push(source_name.clone());
                routes.push(Some(route.clone()));
                names.push(json_string(route_meta, "name"));
                urls.push(json_string(route_meta, "url").or_else(|| json_string(source, "url")));
                categories.push(route_categories.map(|values| values.join(",")));
                heats.push(route_meta.get("heat").and_then(|value| value.as_i64()));
                examples.push(example);
                rsshub_urls.push(rsshub_url);
                descriptions.push(json_string(route_meta, "description"));
                require_configs.push(json_bool(features, "requireConfig"));
                require_puppeteers.push(json_bool(features, "requirePuppeteer"));
                support_radars.push(json_bool(features, "supportRadar"));
                top_feed_urls.push(top_feed_url(route_meta));
            }
        }

        routes_record_batch(RoutesColumns {
            source_keys,
            source_names,
            routes,
            names,
            urls,
            categories,
            heats,
            examples,
            rsshub_urls,
            descriptions,
            require_configs,
            require_puppeteers,
            support_radars,
            top_feed_urls,
        })
    }
}

impl RssSubscription {
    pub fn new(table: impl AsRef<str>, url: impl AsRef<str>) -> Result<Self> {
        let table = normalize_subscription_table(table.as_ref())?;
        let url = url.as_ref().trim().to_string();
        validate_feed_url(&url)?;

        Ok(Self { table, url })
    }

    pub fn table(&self) -> &str {
        &self.table
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn from_json(text: &str) -> Result<Vec<Self>> {
        let configs = serde_json::from_str::<Vec<RssSubscriptionConfig>>(text)
            .map_err(|error| GatewayError::Adapter(error.to_string()))?;
        configs
            .into_iter()
            .map(|config| {
                let table = config.table.or(config.name).ok_or_else(|| {
                    GatewayError::Adapter(
                        "RSS subscription requires a table or name field".to_string(),
                    )
                })?;
                let url = config.url.or(config.feed_url).ok_or_else(|| {
                    GatewayError::Adapter(
                        "RSS subscription requires a url or feed_url field".to_string(),
                    )
                })?;
                Self::new(table, url)
            })
            .collect()
    }
}

fn normalize_subscription_table(value: &str) -> Result<String> {
    let table = value
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
        .collect::<String>()
        .split('_')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("_");

    if table.is_empty() {
        return Err(GatewayError::Adapter(
            "RSS subscription table name must not be empty".to_string(),
        ));
    }

    if matches!(table.as_str(), "routes" | "feed" | "entries") {
        return Err(GatewayError::Adapter(format!(
            "RSS subscription table name '{table}' is reserved"
        )));
    }

    Ok(table)
}

fn validate_feed_url(url: &str) -> Result<()> {
    if url.starts_with("rsshub://") || url.starts_with("http://") || url.starts_with("https://") {
        return Ok(());
    }

    Err(GatewayError::Adapter(format!(
        "RSS subscription URL must start with http://, https://, or rsshub://: {url}"
    )))
}

impl Default for RssAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Adapter for RssAdapter {
    fn name(&self) -> &str {
        "rss"
    }

    fn catalog(&self) -> Vec<TableDef> {
        let mut tables = vec![
            TableDef {
                name: "routes".to_string(),
                schema: Self::routes_schema(),
                kind: TableKind::Table,
            },
            TableDef {
                name: "feed".to_string(),
                schema: Self::feed_schema(),
                kind: TableKind::TableFunction {
                    arg_names: vec!["url".to_string()],
                },
            },
            TableDef {
                name: "entries".to_string(),
                schema: Self::entries_schema(),
                kind: TableKind::TableFunction {
                    arg_names: vec!["url".to_string()],
                },
            },
        ];

        tables.extend(
            DEFAULT_RSSHUB_ROUTE_CARDS
                .iter()
                .map(|route_card| TableDef {
                    name: route_card.table.to_string(),
                    schema: Self::entries_schema(),
                    kind: TableKind::Table,
                }),
        );

        tables.extend(
            self.subscriptions
                .iter()
                .filter(|subscription| default_route_card_url(&subscription.table).is_none())
                .map(|subscription| TableDef {
                    name: subscription.table.clone(),
                    schema: Self::entries_schema(),
                    kind: TableKind::Table,
                }),
        );

        tables
    }

    async fn scan(&self, req: ScanRequest) -> Result<Vec<RecordBatch>> {
        match req.table.as_str() {
            "routes" => self.scan_routes().await,
            "feed" => {
                self.scan_feed(Self::feed_url_arg(&req.tvf_args, "feed")?)
                    .await
            }
            "entries" => {
                self.scan_entries(Self::feed_url_arg(&req.tvf_args, "entries")?)
                    .await
            }
            table => {
                if let Some(url) = self.subscription_feed_url(table) {
                    return self.scan_entries(url).await;
                }
                Err(GatewayError::UnknownTable(req.table))
            }
        }
    }
}

fn default_route_card_url(table: &str) -> Option<&'static str> {
    DEFAULT_RSSHUB_ROUTE_CARDS
        .iter()
        .find_map(|route_card| (route_card.table == table).then_some(route_card.source_url))
}

fn public_instance_route_fallback(route: &str) -> Option<&'static str> {
    DEFAULT_RSSHUB_ROUTE_CARDS
        .iter()
        .find(|route_card| {
            route_card
                .source_url
                .strip_prefix("rsshub://")
                .is_some_and(|source_route| source_route.trim_start_matches('/') == route)
        })
        .and_then(|route_card| route_card.public_instance_fetch_url)
}

fn record_batch(
    schema: Arc<Schema>,
    columns: Vec<Vec<Option<String>>>,
) -> Result<Vec<RecordBatch>> {
    let arrays: Vec<ArrayRef> = columns
        .into_iter()
        .map(|values| Arc::new(StringArray::from(values)) as ArrayRef)
        .collect();
    let batch = RecordBatch::try_new(schema, arrays)
        .map_err(|error| GatewayError::Schema(error.to_string()))?;
    Ok(vec![batch])
}

struct RoutesColumns {
    source_keys: Vec<Option<String>>,
    source_names: Vec<Option<String>>,
    routes: Vec<Option<String>>,
    names: Vec<Option<String>>,
    urls: Vec<Option<String>>,
    categories: Vec<Option<String>>,
    heats: Vec<Option<i64>>,
    examples: Vec<Option<String>>,
    rsshub_urls: Vec<Option<String>>,
    descriptions: Vec<Option<String>>,
    require_configs: Vec<Option<bool>>,
    require_puppeteers: Vec<Option<bool>>,
    support_radars: Vec<Option<bool>>,
    top_feed_urls: Vec<Option<String>>,
}

fn routes_record_batch(columns: RoutesColumns) -> Result<Vec<RecordBatch>> {
    let arrays: Vec<ArrayRef> = vec![
        Arc::new(StringArray::from(columns.source_keys)) as ArrayRef,
        Arc::new(StringArray::from(columns.source_names)) as ArrayRef,
        Arc::new(StringArray::from(columns.routes)) as ArrayRef,
        Arc::new(StringArray::from(columns.names)) as ArrayRef,
        Arc::new(StringArray::from(columns.urls)) as ArrayRef,
        Arc::new(StringArray::from(columns.categories)) as ArrayRef,
        Arc::new(Int64Array::from(columns.heats)) as ArrayRef,
        Arc::new(StringArray::from(columns.examples)) as ArrayRef,
        Arc::new(StringArray::from(columns.rsshub_urls)) as ArrayRef,
        Arc::new(StringArray::from(columns.descriptions)) as ArrayRef,
        Arc::new(BooleanArray::from(columns.require_configs)) as ArrayRef,
        Arc::new(BooleanArray::from(columns.require_puppeteers)) as ArrayRef,
        Arc::new(BooleanArray::from(columns.support_radars)) as ArrayRef,
        Arc::new(StringArray::from(columns.top_feed_urls)) as ArrayRef,
    ];
    let batch = RecordBatch::try_new(RssAdapter::routes_schema(), arrays)
        .map_err(|error| GatewayError::Schema(error.to_string()))?;
    Ok(vec![batch])
}

fn json_string(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

fn json_bool(value: Option<&serde_json::Value>, key: &str) -> Option<bool> {
    value?.get(key).and_then(|value| value.as_bool())
}

fn json_string_array(value: Option<&serde_json::Value>) -> Option<Vec<String>> {
    let values: Vec<String> = value?
        .as_array()?
        .iter()
        .filter_map(|value| value.as_str().map(str::to_string))
        .collect();
    if values.is_empty() {
        None
    } else {
        Some(values)
    }
}

fn top_feed_url(route_meta: &serde_json::Value) -> Option<String> {
    route_meta
        .get("topFeeds")?
        .as_array()?
        .iter()
        .find_map(|feed| json_string(feed, "url"))
}

fn parse_feed(text: &str) -> ParsedFeed {
    if let Some(channel) = first_element_body(text, "channel") {
        return ParsedFeed {
            title: first_tag_text(&channel, "title"),
            description: first_tag_text(&channel, "description"),
            site_url: first_tag_text(&channel, "link"),
            entries: element_bodies(&channel, "item")
                .into_iter()
                .map(parse_rss_item)
                .collect(),
        };
    }

    let feed = first_element_body(text, "feed").unwrap_or_else(|| text.to_string());
    ParsedFeed {
        title: first_tag_text(&feed, "title"),
        description: first_tag_text(&feed, "subtitle"),
        site_url: first_link_href(&feed).or_else(|| first_tag_text(&feed, "link")),
        entries: element_bodies(&feed, "entry")
            .into_iter()
            .map(parse_atom_entry)
            .collect(),
    }
}

fn parse_rss_item(item: String) -> ParsedEntry {
    ParsedEntry {
        guid: first_tag_text(&item, "guid").or_else(|| first_tag_text(&item, "link")),
        title: first_tag_text(&item, "title"),
        url: first_tag_text(&item, "link"),
        description: first_tag_text(&item, "description"),
        published_at: first_tag_text(&item, "pubDate"),
        author: first_tag_text(&item, "author"),
        categories: all_tag_text(&item, "category"),
    }
}

fn parse_atom_entry(entry: String) -> ParsedEntry {
    ParsedEntry {
        guid: first_tag_text(&entry, "id").or_else(|| first_link_href(&entry)),
        title: first_tag_text(&entry, "title"),
        url: first_link_href(&entry).or_else(|| first_tag_text(&entry, "link")),
        description: first_tag_text(&entry, "summary")
            .or_else(|| first_tag_text(&entry, "content")),
        published_at: first_tag_text(&entry, "published")
            .or_else(|| first_tag_text(&entry, "updated")),
        author: first_element_body(&entry, "author")
            .and_then(|author| first_tag_text(&author, "name"))
            .or_else(|| first_tag_text(&entry, "author")),
        categories: category_terms(&entry),
    }
}

fn first_element_body(text: &str, tag: &str) -> Option<String> {
    element_bodies(text, tag).into_iter().next()
}

fn element_bodies(text: &str, tag: &str) -> Vec<String> {
    let mut bodies = Vec::new();
    let mut rest = text;
    let open_prefix = format!("<{tag}");
    let close = format!("</{tag}>");

    while let Some(open_start) = rest.find(&open_prefix) {
        let after_open = &rest[open_start..];
        let Some(open_end) = after_open.find('>') else {
            break;
        };
        let body_start = open_start + open_end + 1;
        let after_body_start = &rest[body_start..];
        let Some(close_start) = after_body_start.find(&close) else {
            break;
        };
        bodies.push(after_body_start[..close_start].to_string());
        let close_end = body_start + close_start + close.len();
        rest = &rest[close_end..];
    }

    bodies
}

fn first_tag_text(text: &str, tag: &str) -> Option<String> {
    all_tag_text(text, tag).into_iter().next()
}

fn all_tag_text(text: &str, tag: &str) -> Vec<String> {
    element_bodies(text, tag)
        .into_iter()
        .filter_map(|value| clean_xml_text(&value))
        .collect()
}

fn first_link_href(text: &str) -> Option<String> {
    let mut rest = text;
    while let Some(start) = rest.find("<link") {
        let after_start = &rest[start..];
        let Some(end) = after_start.find('>') else {
            break;
        };
        let element = &after_start[..end + 1];
        if let Some(href) = attr_value(element, "href") {
            return Some(href);
        }
        rest = &after_start[end + 1..];
    }
    None
}

fn category_terms(text: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("<category") {
        let after_start = &rest[start..];
        let Some(end) = after_start.find('>') else {
            break;
        };
        let element = &after_start[..end + 1];
        if let Some(term) = attr_value(element, "term") {
            terms.push(term);
        }
        rest = &after_start[end + 1..];
    }
    terms
}

fn attr_value(element: &str, attr: &str) -> Option<String> {
    let needle = format!("{attr}=\"");
    let start = element.find(&needle)? + needle.len();
    let end = element[start..].find('"')?;
    clean_xml_text(&element[start..start + end])
}

fn clean_xml_text(value: &str) -> Option<String> {
    let value = value
        .trim()
        .strip_prefix("<![CDATA[")
        .and_then(|inner| inner.strip_suffix("]]>"))
        .unwrap_or_else(|| value.trim())
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'");
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

#[cfg(test)]
mod tests {
    use arrow_array::StringArray;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::{RssAdapter, RssSubscription};
    use crate::adapter::{Adapter, ResolvedAuth, ScalarValue, ScanRequest, TableKind};

    fn scan_request_with_args(table: &str, tvf_args: Vec<ScalarValue>) -> ScanRequest {
        ScanRequest {
            table: table.to_string(),
            predicates: vec![],
            projection: None,
            tvf_args,
            auth: ResolvedAuth::None,
        }
    }

    fn scan_request(table: &str, url: String) -> ScanRequest {
        scan_request_with_args(table, vec![ScalarValue::Utf8(url)])
    }

    fn string_value(batch: &arrow_array::RecordBatch, column: usize, row: usize) -> String {
        batch
            .column(column)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("column should be string")
            .value(row)
            .to_string()
    }

    fn bool_value(batch: &arrow_array::RecordBatch, column: usize, row: usize) -> bool {
        batch
            .column(column)
            .as_any()
            .downcast_ref::<arrow_array::BooleanArray>()
            .expect("column should be bool")
            .value(row)
    }

    fn int_value(batch: &arrow_array::RecordBatch, column: usize, row: usize) -> i64 {
        batch
            .column(column)
            .as_any()
            .downcast_ref::<arrow_array::Int64Array>()
            .expect("column should be int")
            .value(row)
    }

    #[test]
    fn rss_catalog_exposes_routes_feed_and_entries_tables() {
        let adapter = RssAdapter::new();
        let catalog = adapter.catalog();

        assert_eq!(catalog.len(), 4);
        assert!(catalog
            .iter()
            .any(|table| table.name == "routes" && matches!(table.kind, TableKind::Table)));
        assert!(catalog.iter().any(|table| {
            table.name == "hackernews_jobs_entries" && matches!(table.kind, TableKind::Table)
        }));
        assert!(catalog.iter().any(|table| {
            table.name == "feed"
                && matches!(
                    table.kind,
                    TableKind::TableFunction { ref arg_names } if arg_names == &["url".to_string()]
                )
        }));
        assert!(catalog.iter().any(|table| {
            table.name == "entries"
                && matches!(
                    table.kind,
                    TableKind::TableFunction { ref arg_names } if arg_names == &["url".to_string()]
                )
        }));
    }

    #[tokio::test]
    async fn rss_subscription_table_scans_fixed_rsshub_route_entries() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/youtube/channel/UC123"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"<?xml version="1.0"?>
<rss version="2.0">
  <channel>
    <title>Example Channel</title>
    <item>
      <title>First video</title>
      <link>https://example.test/first</link>
      <guid>video-1</guid>
      <pubDate>Mon, 01 Jan 2024 00:00:00 GMT</pubDate>
    </item>
  </channel>
</rss>"#,
            ))
            .mount(&server)
            .await;

        let adapter = RssAdapter::with_config(&server.uri(), "https://example.test/routes.json")
            .with_subscriptions(vec![RssSubscription::new(
                "youtube_channel_entries",
                "rsshub://youtube/channel/UC123",
            )
            .expect("subscription should be valid")]);
        let catalog = adapter.catalog();

        assert!(catalog.iter().any(|table| {
            table.name == "youtube_channel_entries" && matches!(table.kind, TableKind::Table)
        }));

        let batches = adapter
            .scan(scan_request_with_args("youtube_channel_entries", vec![]))
            .await
            .expect("subscription scan succeeds");

        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].num_rows(), 1);
        assert_eq!(
            string_value(&batches[0], 0, 0),
            "rsshub://youtube/channel/UC123"
        );
        assert_eq!(string_value(&batches[0], 1, 0), "video-1");
        assert_eq!(string_value(&batches[0], 2, 0), "First video");
    }

    #[tokio::test]
    async fn rss_default_route_card_table_scans_hackernews_jobs_entries() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/hackernews/jobs"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"<?xml version="1.0"?>
<rss version="2.0">
  <channel>
    <title>Hacker News Jobs</title>
    <item>
      <title>Data systems engineer</title>
      <link>https://news.ycombinator.com/item?id=1</link>
      <guid>job-1</guid>
    </item>
  </channel>
</rss>"#,
            ))
            .mount(&server)
            .await;

        let adapter = RssAdapter::with_config(&server.uri(), "https://example.test/routes.json");
        let catalog = adapter.catalog();

        assert!(catalog.iter().any(|table| {
            table.name == "hackernews_jobs_entries" && matches!(table.kind, TableKind::Table)
        }));

        let batches = adapter
            .scan(scan_request_with_args("hackernews_jobs_entries", vec![]))
            .await
            .expect("default route-card scan succeeds");

        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].num_rows(), 1);
        assert_eq!(string_value(&batches[0], 0, 0), "rsshub://hackernews/jobs");
        assert_eq!(string_value(&batches[0], 1, 0), "job-1");
        assert_eq!(string_value(&batches[0], 2, 0), "Data systems engineer");
    }

    #[tokio::test]
    async fn rss_routes_scan_flattens_rsshub_route_catalog() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/routes.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "youtube": {
                    "name": "YouTube",
                    "url": "youtube.com",
                    "categories": ["multimedia", "popular"],
                    "routes": {
                        "/youtube/video/:id": {
                            "path": "/video/:id",
                            "name": "Channel videos",
                            "url": "youtube.com",
                            "example": "/youtube/video/UC123",
                            "parameters": {
                                "id": "Channel ID"
                            },
                            "description": "Latest videos",
                            "categories": ["multimedia"],
                            "features": {
                                "requireConfig": false,
                                "requirePuppeteer": true,
                                "supportRadar": true
                            },
                            "heat": 42,
                            "topFeeds": [
                                {
                                    "url": "rsshub://youtube/video/UC123",
                                    "title": "Example channel"
                                }
                            ]
                        }
                    }
                }
            })))
            .mount(&server)
            .await;

        let batches = RssAdapter::with_config(
            "https://rsshub.example",
            &format!("{}/routes.json", server.uri()),
        )
        .scan(scan_request_with_args("routes", vec![]))
        .await
        .expect("routes scan succeeds");

        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].num_rows(), 1);
        assert_eq!(string_value(&batches[0], 0, 0), "youtube");
        assert_eq!(string_value(&batches[0], 1, 0), "YouTube");
        assert_eq!(string_value(&batches[0], 2, 0), "/youtube/video/:id");
        assert_eq!(string_value(&batches[0], 3, 0), "Channel videos");
        assert_eq!(string_value(&batches[0], 4, 0), "youtube.com");
        assert_eq!(string_value(&batches[0], 5, 0), "multimedia");
        assert_eq!(int_value(&batches[0], 6, 0), 42);
        assert_eq!(string_value(&batches[0], 7, 0), "/youtube/video/UC123");
        assert_eq!(
            string_value(&batches[0], 8, 0),
            "rsshub://youtube/video/UC123"
        );
        assert_eq!(string_value(&batches[0], 9, 0), "Latest videos");
        assert!(!bool_value(&batches[0], 10, 0));
        assert!(bool_value(&batches[0], 11, 0));
        assert!(bool_value(&batches[0], 12, 0));
        assert_eq!(
            string_value(&batches[0], 13, 0),
            "rsshub://youtube/video/UC123"
        );
    }

    #[tokio::test]
    async fn rss_feed_scan_parses_channel_metadata() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/feed.xml"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"<?xml version="1.0"?>
<rss version="2.0">
  <channel>
    <title>Example Feed</title>
    <description>Stories from example</description>
    <link>https://example.test/</link>
    <item>
      <title>First</title>
      <link>https://example.test/first</link>
      <guid>first-guid</guid>
    </item>
  </channel>
</rss>"#,
            ))
            .mount(&server)
            .await;

        let batches = RssAdapter::new()
            .scan(scan_request("feed", format!("{}/feed.xml", server.uri())))
            .await
            .expect("feed scan succeeds");

        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].num_rows(), 1);
        assert_eq!(
            string_value(&batches[0], 0, 0),
            format!("{}/feed.xml", server.uri())
        );
        assert_eq!(string_value(&batches[0], 1, 0), "Example Feed");
        assert_eq!(string_value(&batches[0], 2, 0), "Stories from example");
        assert_eq!(string_value(&batches[0], 3, 0), "https://example.test/");
    }

    #[tokio::test]
    async fn rss_entries_scan_parses_items() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/feed.xml"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"<?xml version="1.0"?>
<rss version="2.0">
  <channel>
    <title>Example Feed</title>
    <item>
      <title>First</title>
      <link>https://example.test/first</link>
      <guid>first-guid</guid>
      <description>First story</description>
      <pubDate>Sat, 13 Jun 2026 09:00:00 GMT</pubDate>
      <author>editor@example.test</author>
      <category>AI</category>
      <category>News</category>
    </item>
  </channel>
</rss>"#,
            ))
            .mount(&server)
            .await;

        let feed_url = format!("{}/feed.xml", server.uri());
        let batches = RssAdapter::new()
            .scan(scan_request("entries", feed_url.clone()))
            .await
            .expect("entries scan succeeds");

        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].num_rows(), 1);
        assert_eq!(string_value(&batches[0], 0, 0), feed_url);
        assert_eq!(string_value(&batches[0], 1, 0), "first-guid");
        assert_eq!(string_value(&batches[0], 2, 0), "First");
        assert_eq!(
            string_value(&batches[0], 3, 0),
            "https://example.test/first"
        );
        assert_eq!(string_value(&batches[0], 4, 0), "First story");
        assert_eq!(
            string_value(&batches[0], 5, 0),
            "Sat, 13 Jun 2026 09:00:00 GMT"
        );
        assert_eq!(string_value(&batches[0], 6, 0), "editor@example.test");
        assert_eq!(string_value(&batches[0], 7, 0), "AI,News");
    }

    #[test]
    fn public_rsshub_hackernews_jobs_uses_direct_feed_fallback() {
        let default_adapter = RssAdapter::new();
        assert_eq!(
            default_adapter
                .resolved_fetch_url("rsshub://hackernews/jobs")
                .expect("default route resolves"),
            "https://hnrss.org/jobs"
        );

        let self_hosted_adapter = RssAdapter::with_rsshub_base("https://rsshub.example");
        assert_eq!(
            self_hosted_adapter
                .resolved_fetch_url("rsshub://hackernews/jobs")
                .expect("self-hosted route resolves"),
            "https://rsshub.example/hackernews/jobs"
        );
    }

    #[tokio::test]
    async fn rsshub_url_resolves_through_configured_base() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/youtube/video/UC123"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"<?xml version="1.0"?>
<rss version="2.0">
  <channel>
    <title>YouTube Feed</title>
    <description>Videos</description>
    <link>https://youtube.test/channel/UC123</link>
  </channel>
</rss>"#,
            ))
            .mount(&server)
            .await;

        let batches = RssAdapter::with_rsshub_base(&server.uri())
            .scan(scan_request(
                "feed",
                "rsshub://youtube/video/UC123".to_string(),
            ))
            .await
            .expect("rsshub scan succeeds");

        assert_eq!(batches[0].num_rows(), 1);
        assert_eq!(
            string_value(&batches[0], 0, 0),
            "rsshub://youtube/video/UC123"
        );
        assert_eq!(string_value(&batches[0], 1, 0), "YouTube Feed");
    }
}
