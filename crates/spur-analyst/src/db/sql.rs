#[cfg(test)]
use std::path::Path;

pub(crate) fn sql_escape_literal(value: &str) -> String {
    value.replace('\'', "''")
}

pub(crate) fn sql_string_literal(value: &str) -> String {
    format!("'{}'", sql_escape_literal(value))
}

#[cfg(test)]
pub(crate) fn sql_escape_path(path: &Path) -> String {
    sql_escape_literal(&path.display().to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn db_sql_string_literal_wraps_and_escapes_quotes() {
        assert_eq!(super::sql_string_literal("O'Malley"), "'O''Malley'");
    }
}
