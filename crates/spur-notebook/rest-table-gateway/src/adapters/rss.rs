use std::sync::Arc;

use arrow_array::{ArrayRef, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use async_trait::async_trait;
use reqwest::Client;

use crate::adapter::{Adapter, ScalarValue, ScanRequest, TableDef, TableKind};
use crate::error::{GatewayError, Result};

const DEFAULT_RSSHUB_BASE: &str = "https://rsshub.app";

pub struct RssAdapter {
    rsshub_base: String,
    client: Client,
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
        Self {
            rsshub_base: rsshub_base.trim_end_matches('/').to_string(),
            client: crate::adapter::default_http_client(),
        }
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
        let response = self
            .client
            .get(fetch_url)
            .send()
            .await
            .map_err(|error| GatewayError::Http(error.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            return Err(GatewayError::Http(format!(
                "RSS feed request failed with HTTP {status}"
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
        vec![
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
        ]
    }

    async fn scan(&self, req: ScanRequest) -> Result<Vec<RecordBatch>> {
        match req.table.as_str() {
            "feed" => {
                self.scan_feed(Self::feed_url_arg(&req.tvf_args, "feed")?)
                    .await
            }
            "entries" => {
                self.scan_entries(Self::feed_url_arg(&req.tvf_args, "entries")?)
                    .await
            }
            _ => Err(GatewayError::UnknownTable(req.table)),
        }
    }
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

    use super::RssAdapter;
    use crate::adapter::{Adapter, ResolvedAuth, ScalarValue, ScanRequest, TableKind};

    fn scan_request(table: &str, url: String) -> ScanRequest {
        ScanRequest {
            table: table.to_string(),
            predicates: vec![],
            projection: None,
            tvf_args: vec![ScalarValue::Utf8(url)],
            auth: ResolvedAuth::None,
        }
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

    #[test]
    fn rss_catalog_exposes_feed_and_entries_functions() {
        let adapter = RssAdapter::new();
        let catalog = adapter.catalog();

        assert_eq!(catalog.len(), 2);
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
