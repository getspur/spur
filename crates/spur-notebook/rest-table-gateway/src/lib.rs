//! spur-rest-table-gateway: map REST/GraphQL APIs to SQL tables — turns a ScanRequest into Arrow record batches behind an Adapter trait.
pub mod adapter;
pub mod adapters;
pub mod error;
