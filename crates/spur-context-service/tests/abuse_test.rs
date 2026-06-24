use spur_context_service::abuse::{
    resolve_and_check_dns, validate, AbuseError, ParsedSourceUrl, RateLimiter, SourceKind,
    ValidateOptions,
};

fn default_opts() -> ValidateOptions {
    ValidateOptions::default()
}

fn parsed_for_host(hostname: &str) -> ParsedSourceUrl {
    ParsedSourceUrl {
        source_kind: SourceKind::Tarball,
        hostname: hostname.to_string(),
        url: format!("https://{hostname}/archive.tar.gz"),
    }
}

#[test]
fn abuse_rejects_http_scheme() {
    let err = validate("http://example.com/archive.tar.gz", &default_opts()).unwrap_err();

    assert_eq!(err, AbuseError::ForbiddenScheme);
}

#[test]
fn abuse_accepts_https_git_https_and_git_ssh_schemes() {
    assert!(validate("https://example.com/archive.tar.gz", &default_opts()).is_ok());
    assert!(validate("git+https://github.com/getspur/spur.git", &default_opts()).is_ok());
    assert!(validate("git+ssh://git@github.com/getspur/spur.git", &default_opts()).is_ok());
}

#[test]
fn abuse_infers_source_kind_from_url_suffix() {
    let git = validate("https://github.com/getspur/spur.git", &default_opts()).unwrap();
    let tarball = validate("https://example.com/spur.tar.gz", &default_opts()).unwrap();

    assert_eq!(git.source_kind, SourceKind::Git);
    assert_eq!(tarball.source_kind, SourceKind::Tarball);
}

#[test]
fn abuse_allowlist_empty_allows_public_domains() {
    let parsed = validate("https://example.net/archive.tar.gz", &default_opts()).unwrap();

    assert_eq!(parsed.hostname, "example.net");
}

#[test]
fn abuse_allowlist_populated_rejects_non_matching_domains() {
    let opts = ValidateOptions {
        allowed_domains: vec!["github.com".to_string()],
        ..ValidateOptions::default()
    };

    let err = validate("https://example.com/archive.tar.gz", &opts).unwrap_err();

    assert_eq!(err, AbuseError::DomainNotAllowlisted);
}

#[test]
fn abuse_allowlist_matches_domain_suffix_boundaries() {
    let opts = ValidateOptions {
        allowed_domains: vec!["crates.io".to_string()],
        ..ValidateOptions::default()
    };

    assert!(validate("https://static.crates.io/archive.tar.gz", &opts).is_ok());

    let err = validate("https://evilcrates.io/archive.tar.gz", &opts).unwrap_err();
    assert_eq!(err, AbuseError::DomainNotAllowlisted);
}

#[test]
fn abuse_rejects_localhost_without_dns_resolution() {
    let err = validate("https://localhost/archive.tar.gz", &default_opts()).unwrap_err();

    assert_eq!(err, AbuseError::Localhost);
}

#[test]
fn abuse_rejects_link_local_ip_literals() {
    let err = validate("https://169.254.1.10/archive.tar.gz", &default_opts()).unwrap_err();

    assert_eq!(err, AbuseError::LinkLocal);
}

#[test]
fn abuse_rejects_aws_metadata_ip_literals() {
    let v4 = validate(
        "https://169.254.169.254/latest/meta-data/archive.tar.gz",
        &default_opts(),
    )
    .unwrap_err();
    let v6 = validate("https://[fd00:ec2::254]/archive.tar.gz", &default_opts()).unwrap_err();

    assert_eq!(v4, AbuseError::AwsMetadata);
    assert_eq!(v6, AbuseError::AwsMetadata);
}

#[test]
fn abuse_rejects_rfc1918_ip_literals() {
    for url in [
        "https://10.1.2.3/archive.tar.gz",
        "https://172.16.0.1/archive.tar.gz",
        "https://172.31.255.255/archive.tar.gz",
        "https://192.168.1.1/archive.tar.gz",
    ] {
        let err = validate(url, &default_opts()).unwrap_err();
        assert_eq!(err, AbuseError::PrivateRangeRfc1918);
    }
}

#[test]
fn abuse_dns_check_rejects_forbidden_ip_ranges() {
    assert_eq!(
        resolve_and_check_dns(&parsed_for_host("127.0.0.8")).unwrap_err(),
        AbuseError::Localhost
    );
    assert_eq!(
        resolve_and_check_dns(&parsed_for_host("169.254.42.42")).unwrap_err(),
        AbuseError::LinkLocal
    );
    assert_eq!(
        resolve_and_check_dns(&parsed_for_host("169.254.169.254")).unwrap_err(),
        AbuseError::AwsMetadata
    );
    assert_eq!(
        resolve_and_check_dns(&parsed_for_host("fd00:ec2::254")).unwrap_err(),
        AbuseError::AwsMetadata
    );
    assert_eq!(
        resolve_and_check_dns(&parsed_for_host("10.0.0.10")).unwrap_err(),
        AbuseError::PrivateRangeRfc1918
    );
}

#[test]
fn abuse_rejects_tarball_size_hints_over_cap() {
    let opts = ValidateOptions {
        tarball_size_cap_bytes: 10,
        ..ValidateOptions::default()
    };

    let err = validate("https://example.com/archive.tar.gz?size=11", &opts).unwrap_err();

    assert_eq!(err, AbuseError::SizeCapExceeded);
}

#[test]
fn abuse_rejects_git_size_hints_over_cap() {
    let opts = ValidateOptions {
        git_size_cap_bytes: 10,
        ..ValidateOptions::default()
    };

    let err = validate("https://github.com/getspur/spur.git?size=11", &opts).unwrap_err();

    assert_eq!(err, AbuseError::SizeCapExceeded);
}

#[test]
fn abuse_rate_limiter_uses_default_ten_calls_per_minute() {
    let limiter = RateLimiter::default();

    for _ in 0..10 {
        limiter.check("caller-a").unwrap();
    }

    assert!(limiter.check("caller-a").is_err());
    assert!(limiter.check("caller-b").is_ok());
}

#[test]
fn abuse_rate_limiter_honors_custom_calls_per_minute() {
    let limiter = RateLimiter::new(2);

    limiter.check("caller").unwrap();
    limiter.check("caller").unwrap();

    assert!(limiter.check("caller").is_err());
}
