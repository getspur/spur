#![expect(
    clippy::allow_attributes,
    reason = "legacy bot modules still contain localized allow attributes"
)]
#![expect(
    clippy::doc_markdown,
    reason = "legacy bot docs contain Telegram/ACP terms that are not consistently backticked yet"
)]
#![expect(
    clippy::format_push_string,
    reason = "legacy bot rendering appends formatted strings directly"
)]
#![expect(
    clippy::ignored_unit_patterns,
    reason = "legacy bot select branches use wildcard unit patterns"
)]
#![expect(
    clippy::implicit_clone,
    reason = "legacy bot code sometimes clones strings through to_string"
)]
#![expect(
    clippy::iter_over_hash_type,
    reason = "legacy bot runtime iterates hash maps while rebuilding chat thread state"
)]
#![expect(
    clippy::map_err_ignore,
    reason = "legacy bot channel error mapping intentionally discards source errors"
)]
#![expect(
    clippy::match_same_arms,
    reason = "legacy bot event rendering keeps no-op and equivalent arms explicit"
)]
#![expect(
    clippy::missing_assert_message,
    reason = "legacy bot formatter debug assertions omit explicit messages"
)]
#![expect(
    clippy::ref_patterns,
    reason = "legacy bot code still uses explicit ref bindings"
)]
#![expect(
    clippy::return_and_then,
    reason = "legacy bot Telegram response extraction uses and_then chains"
)]
#![expect(
    clippy::semicolon_if_nothing_returned,
    reason = "legacy bot task callbacks omit semicolons in unit-returning expressions"
)]
#![expect(
    clippy::str_to_string,
    reason = "legacy bot code has many &str to String conversions pending mechanical cleanup"
)]
#![expect(
    clippy::uninlined_format_args,
    reason = "legacy bot formatting has not all moved to captured format args"
)]
#![expect(
    clippy::unused_trait_names,
    reason = "legacy bot modules import extension traits by name"
)]

pub mod commands;
pub mod runtime;
pub mod state;
pub mod telegram;
