//! Context refs — the one reference convention from the context-provider spec §3.
//! v1 refs are notebook-relative; the notebook scope comes from the tool's
//! `notebook_path`/daemon current path.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ref {
    Datasource { id: String, table: Option<String> },
    Cell { id: String, version: Option<u64> },
    Port { name: String, version: Option<u64> },
    Symbol { cell_id: String, name: String },
}

#[derive(Debug, thiserror::Error)]
pub enum RefError {
    #[error("unknown ref scheme: {0}")]
    UnknownScheme(String),
    #[error("bad version anchor: {0}")]
    BadVersion(String),
    #[error("empty ref body")]
    Empty,
}

impl Ref {
    pub fn parse(raw: &str) -> Result<Self, RefError> {
        let (scheme, body) = raw
            .split_once("://")
            .ok_or_else(|| RefError::UnknownScheme(raw.to_owned()))?;
        if body.is_empty() {
            return Err(RefError::Empty);
        }
        let (body, version) = split_version(body)?;
        match scheme {
            "ds" => {
                let (id, table) = match body.split_once('/') {
                    Some((id, table)) => (id.to_owned(), Some(table.to_owned())),
                    None => (body.to_owned(), None),
                };
                Ok(Ref::Datasource { id, table })
            }
            "cell" => Ok(Ref::Cell {
                id: body.to_owned(),
                version,
            }),
            "port" => Ok(Ref::Port {
                name: body.to_owned(),
                version,
            }),
            "sym" => {
                let (cell_id, name) = body
                    .split_once('/')
                    .ok_or_else(|| RefError::UnknownScheme(raw.to_owned()))?;
                Ok(Ref::Symbol {
                    cell_id: cell_id.to_owned(),
                    name: name.to_owned(),
                })
            }
            other => Err(RefError::UnknownScheme(other.to_owned())),
        }
    }
}

fn split_version(body: &str) -> Result<(&str, Option<u64>), RefError> {
    match body.rsplit_once("@v") {
        Some((head, tail)) => {
            let version = tail
                .parse::<u64>()
                .map_err(|_| RefError::BadVersion(body.to_owned()))?;
            Ok((head, Some(version)))
        }
        None => Ok((body, None)),
    }
}

impl fmt::Display for Ref {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ref::Datasource {
                id,
                table: Some(table),
            } => write!(f, "ds://{id}/{table}"),
            Ref::Datasource { id, table: None } => write!(f, "ds://{id}"),
            Ref::Cell {
                id,
                version: Some(v),
            } => write!(f, "cell://{id}@v{v}"),
            Ref::Cell { id, version: None } => write!(f, "cell://{id}"),
            Ref::Port {
                name,
                version: Some(v),
            } => write!(f, "port://{name}@v{v}"),
            Ref::Port {
                name,
                version: None,
            } => write!(f, "port://{name}"),
            Ref::Symbol { cell_id, name } => write!(f, "sym://{cell_id}/{name}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_all_schemes() {
        for raw in [
            "ds://polymarket",
            "ds://polymarket/markets",
            "cell://a3f1",
            "cell://a3f1@v7",
            "port://markets",
            "port://markets@v12",
            "sym://a3f1/load_df",
        ] {
            let parsed = Ref::parse(raw).expect(raw);
            assert_eq!(parsed.to_string(), raw);
        }
    }

    #[test]
    fn rejects_unknown_scheme_and_bad_version() {
        assert!(matches!(
            Ref::parse("http://x"),
            Err(RefError::UnknownScheme(_))
        ));
        assert!(matches!(
            Ref::parse("cell://a@vx"),
            Err(RefError::BadVersion(_))
        ));
        assert!(matches!(Ref::parse("ds://"), Err(RefError::Empty)));
    }
}
