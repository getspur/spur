//! Shared source URL validation and source-kind inference.

use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs as _};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const DEFAULT_TARBALL_SIZE_CAP_BYTES: u64 = 500_u64 * 1024 * 1024;
const DEFAULT_GIT_SIZE_CAP_BYTES: u64 = 2_u64 * 1024 * 1024 * 1024;
const DEFAULT_CALLS_PER_MINUTE: u32 = 10;
const SECONDS_PER_MINUTE: f64 = 60.0;

/// Validation options for an agent-supplied source URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidateOptions {
    /// Maximum accepted tarball size hint.
    pub tarball_size_cap_bytes: u64,
    /// Maximum accepted git clone size hint.
    pub git_size_cap_bytes: u64,
    /// Allowed domain suffixes. Empty means all public domains are allowed.
    pub allowed_domains: Vec<String>,
}

impl Default for ValidateOptions {
    fn default() -> Self {
        Self {
            tarball_size_cap_bytes: DEFAULT_TARBALL_SIZE_CAP_BYTES,
            git_size_cap_bytes: DEFAULT_GIT_SIZE_CAP_BYTES,
            allowed_domains: Vec::new(),
        }
    }
}

/// Source fetch strategy inferred from the URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    /// Fetch with git.
    Git,
    /// Fetch as an archive tarball/zip.
    Tarball,
}

/// Parsed and pre-validated source URL details.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSourceUrl {
    /// Inferred source kind.
    pub source_kind: SourceKind,
    /// Lower-cased hostname without userinfo, port, IPv6 brackets, or trailing dot.
    pub hostname: String,
    /// Original URL string.
    pub url: String,
}

/// Source URL abuse rejection reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbuseError {
    ForbiddenScheme,
    ForbiddenIpRange,
    LinkLocal,
    AwsMetadata,
    PrivateRangeRfc1918,
    Localhost,
    DomainNotAllowlisted,
    SizeCapExceeded,
}

impl fmt::Display for AbuseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ForbiddenScheme => f.write_str("source_url scheme is not allowed"),
            Self::ForbiddenIpRange => f.write_str("source_url resolves to a forbidden IP range"),
            Self::LinkLocal => f.write_str("source_url resolves to a link-local address"),
            Self::AwsMetadata => f.write_str("source_url targets the AWS metadata service"),
            Self::PrivateRangeRfc1918 => {
                f.write_str("source_url resolves to an RFC1918 private address")
            }
            Self::Localhost => f.write_str("source_url targets localhost"),
            Self::DomainNotAllowlisted => f.write_str("source_url domain is not allow-listed"),
            Self::SizeCapExceeded => f.write_str("source_url size hint exceeds the configured cap"),
        }
    }
}

impl Error for AbuseError {}

/// Validate pure URL properties without performing DNS resolution.
pub fn validate(source_url: &str, opts: &ValidateOptions) -> Result<ParsedSourceUrl, AbuseError> {
    let raw = parse_source_url(source_url)?;
    let source_kind = infer_source_kind(&raw.scheme, &raw.hostname, raw.path);

    if is_localhost_name(&raw.hostname) {
        return Err(AbuseError::Localhost);
    }

    if let Ok(ip) = raw.hostname.parse::<IpAddr>() {
        reject_forbidden_ip(ip)?;
    }

    check_size_hint(raw.query, source_kind, opts)?;
    check_allowlist(&raw.hostname, &opts.allowed_domains)?;

    Ok(ParsedSourceUrl {
        source_kind,
        hostname: raw.hostname,
        url: source_url.to_owned(),
    })
}

/// Resolve the hostname and reject DNS answers in forbidden address ranges.
pub fn resolve_and_check_dns(parsed: &ParsedSourceUrl) -> Result<(), AbuseError> {
    if let Ok(ip) = parsed.hostname.parse::<IpAddr>() {
        return reject_forbidden_ip(ip);
    }

    let addrs = (parsed.hostname.as_str(), 443)
        .to_socket_addrs()
        .map_err(|_error| AbuseError::ForbiddenIpRange)?;

    let mut saw_addr = false;
    for addr in addrs {
        saw_addr = true;
        reject_forbidden_ip(addr.ip())?;
    }

    if saw_addr {
        Ok(())
    } else {
        Err(AbuseError::ForbiddenIpRange)
    }
}

/// Rate-limit rejection reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateLimitError {
    LimitExceeded,
}

impl fmt::Display for RateLimitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LimitExceeded => f.write_str("caller exceeded the indexing rate limit"),
        }
    }
}

impl Error for RateLimitError {}

/// In-memory per-caller token bucket rate limiter.
#[derive(Debug)]
pub struct RateLimiter {
    calls_per_minute: u32,
    buckets: Mutex<HashMap<String, TokenBucket>>,
}

impl RateLimiter {
    /// Create a limiter with the provided calls-per-minute capacity.
    pub fn new(calls_per_minute: u32) -> Self {
        Self {
            calls_per_minute,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Consume one token for the caller, or reject if the caller is over limit.
    pub fn check(&self, caller_id: &str) -> Result<(), RateLimitError> {
        let now = Instant::now();
        let capacity = f64::from(self.calls_per_minute);
        let mut buckets = self.buckets.lock().expect("rate limiter mutex poisoned");
        let bucket = buckets
            .entry(caller_id.to_owned())
            .or_insert_with(|| TokenBucket {
                tokens: capacity,
                last_refill: now,
            });

        refill_bucket(bucket, capacity, now);

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            Ok(())
        } else {
            Err(RateLimitError::LimitExceeded)
        }
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new(DEFAULT_CALLS_PER_MINUTE)
    }
}

#[derive(Debug, Clone)]
struct TokenBucket {
    tokens: f64,
    last_refill: Instant,
}

fn refill_bucket(bucket: &mut TokenBucket, capacity: f64, now: Instant) {
    if capacity == 0.0 {
        bucket.tokens = 0.0;
        bucket.last_refill = now;
        return;
    }

    let elapsed = now.saturating_duration_since(bucket.last_refill);
    if elapsed == Duration::ZERO {
        return;
    }

    let refill = elapsed.as_secs_f64() * capacity / SECONDS_PER_MINUTE;
    bucket.tokens = (bucket.tokens + refill).min(capacity);
    bucket.last_refill = now;
}

struct RawSourceUrl<'a> {
    scheme: String,
    hostname: String,
    path: &'a str,
    query: Option<&'a str>,
}

fn parse_source_url(source_url: &str) -> Result<RawSourceUrl<'_>, AbuseError> {
    let (scheme, rest) = source_url
        .split_once("://")
        .ok_or(AbuseError::ForbiddenScheme)?;
    let scheme = scheme.to_ascii_lowercase();

    if !matches!(scheme.as_str(), "https" | "git+https" | "git+ssh") {
        return Err(AbuseError::ForbiddenScheme);
    }

    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    let hostname = extract_hostname(authority)?;
    let tail_without_fragment = rest[authority_end..]
        .split_once('#')
        .map_or(&rest[authority_end..], |(tail, _)| tail);
    let (path, query) = tail_without_fragment
        .split_once('?')
        .map_or((tail_without_fragment, None), |(path, query)| {
            (path, Some(query))
        });

    Ok(RawSourceUrl {
        scheme,
        hostname,
        path,
        query,
    })
}

fn extract_hostname(authority: &str) -> Result<String, AbuseError> {
    if authority.is_empty() {
        return Err(AbuseError::ForbiddenScheme);
    }

    let host_port = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host_port)| host_port);

    if let Some(bracketed) = host_port.strip_prefix('[') {
        let Some(end) = bracketed.find(']') else {
            return Err(AbuseError::ForbiddenScheme);
        };
        let host = &bracketed[..end];
        let suffix = &bracketed[end + 1..];
        if !suffix.is_empty() && !suffix.starts_with(':') {
            return Err(AbuseError::ForbiddenScheme);
        }
        return normalize_hostname(host);
    }

    let host = host_port
        .split_once(':')
        .map_or(host_port, |(host, _)| host);
    normalize_hostname(host)
}

fn normalize_hostname(host: &str) -> Result<String, AbuseError> {
    let hostname = host.trim_end_matches('.').to_ascii_lowercase();
    if hostname.is_empty() || hostname.chars().any(char::is_whitespace) {
        return Err(AbuseError::ForbiddenScheme);
    }
    Ok(hostname)
}

fn infer_source_kind(scheme: &str, hostname: &str, path: &str) -> SourceKind {
    if matches!(scheme, "git+https" | "git+ssh") {
        return SourceKind::Git;
    }

    let normalized_path = path.trim_end_matches('/').to_ascii_lowercase();
    if is_tarball_path(&normalized_path) {
        return SourceKind::Tarball;
    }

    if normalized_path.ends_with(".git") || hostname == "github.com" {
        SourceKind::Git
    } else {
        SourceKind::Tarball
    }
}

fn is_tarball_path(path: &str) -> bool {
    path.ends_with(".tar.gz") || path.ends_with(".tgz") || path.ends_with(".zip")
}

fn is_localhost_name(hostname: &str) -> bool {
    hostname == "localhost" || hostname.ends_with(".localhost")
}

fn check_size_hint(
    query: Option<&str>,
    source_kind: SourceKind,
    opts: &ValidateOptions,
) -> Result<(), AbuseError> {
    let Some(query) = query else {
        return Ok(());
    };
    let cap = match source_kind {
        SourceKind::Git => opts.git_size_cap_bytes,
        SourceKind::Tarball => opts.tarball_size_cap_bytes,
    };

    for pair in query.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if !is_size_hint_key(key) {
            continue;
        }
        let size = value
            .parse::<u64>()
            .map_err(|_error| AbuseError::SizeCapExceeded)?;
        if size > cap {
            return Err(AbuseError::SizeCapExceeded);
        }
    }

    Ok(())
}

fn is_size_hint_key(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "size"
            | "bytes"
            | "size_bytes"
            | "download_size"
            | "content-length"
            | "content_length"
            | "contentlength"
            | "content-length-bytes"
            | "content_length_bytes"
    )
}

fn check_allowlist(hostname: &str, allowed_domains: &[String]) -> Result<(), AbuseError> {
    if allowed_domains.is_empty() {
        return Ok(());
    }

    if allowed_domains
        .iter()
        .any(|domain| domain_suffix_matches(hostname, domain))
    {
        Ok(())
    } else {
        Err(AbuseError::DomainNotAllowlisted)
    }
}

fn domain_suffix_matches(hostname: &str, allowed_domain: &str) -> bool {
    let domain = allowed_domain
        .trim()
        .trim_start_matches('.')
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if domain.is_empty() {
        return false;
    }

    hostname == domain || hostname.ends_with(&format!(".{domain}"))
}

fn reject_forbidden_ip(ip: IpAddr) -> Result<(), AbuseError> {
    match ip {
        IpAddr::V4(ip) => reject_forbidden_ipv4(ip),
        IpAddr::V6(ip) => reject_forbidden_ipv6(ip),
    }
}

fn reject_forbidden_ipv4(ip: Ipv4Addr) -> Result<(), AbuseError> {
    let octets = ip.octets();

    if octets[0] == 127 {
        return Err(AbuseError::Localhost);
    }
    if octets == [169, 254, 169, 254] {
        return Err(AbuseError::AwsMetadata);
    }
    if octets[0] == 169 && octets[1] == 254 {
        return Err(AbuseError::LinkLocal);
    }
    if octets[0] == 10
        || (octets[0] == 172 && (16..=31).contains(&octets[1]))
        || (octets[0] == 192 && octets[1] == 168)
    {
        return Err(AbuseError::PrivateRangeRfc1918);
    }
    if ip.is_unspecified() || ip.is_multicast() || octets == [255, 255, 255, 255] {
        return Err(AbuseError::ForbiddenIpRange);
    }

    Ok(())
}

fn reject_forbidden_ipv6(ip: Ipv6Addr) -> Result<(), AbuseError> {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return reject_forbidden_ipv4(v4);
    }

    let segments = ip.segments();
    if ip == Ipv6Addr::LOCALHOST {
        return Err(AbuseError::Localhost);
    }
    if segments[0] == 0xfd00 {
        return Err(AbuseError::AwsMetadata);
    }
    if (segments[0] & 0xffc0) == 0xfe80 {
        return Err(AbuseError::LinkLocal);
    }
    if ip.is_unspecified() || ip.is_multicast() || (segments[0] & 0xfe00) == 0xfc00 {
        return Err(AbuseError::ForbiddenIpRange);
    }

    Ok(())
}
