use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::types::{Value as SqlValue, ValueRef};
use rusqlite::{Connection, Statement};

use super::{CapabilityProvider, CapabilityProviderError};

#[derive(Debug)]
pub struct SqliteCapabilityProvider {
    database_path: PathBuf,
    connection: Mutex<Connection>,
}

impl SqliteCapabilityProvider {
    pub fn new(database_path: impl Into<PathBuf>) -> Result<Self, CapabilityProviderError> {
        let database_path = database_path.into();
        let connection = Connection::open(&database_path)?;
        Ok(Self {
            database_path,
            connection: Mutex::new(connection),
        })
    }

    pub fn database_path(&self) -> &Path {
        &self.database_path
    }

    pub fn execute(
        &self,
        sql: &str,
        params_json: Option<&str>,
    ) -> Result<String, CapabilityProviderError> {
        let bound_params = parse_sqlite_params(params_json)?;
        let connection = self
            .connection
            .lock()
            .map_err(|_| CapabilityProviderError::new("sqlite connection mutex is poisoned"))?;
        let mut statement = connection.prepare(sql)?;
        bind_statement(&mut statement, &bound_params)?;

        if statement.readonly() {
            let column_names = statement
                .column_names()
                .into_iter()
                .map(|name| name.to_owned())
                .collect::<Vec<_>>();
            let mut rows = statement.raw_query();
            let mut result_rows = Vec::new();

            while let Some(row) = rows.next()? {
                let mut values = serde_json::Map::new();
                for (index, column_name) in column_names.iter().enumerate() {
                    let value = match row.get_ref(index)? {
                        ValueRef::Null => serde_json::Value::Null,
                        ValueRef::Integer(value) => serde_json::Value::from(value),
                        ValueRef::Real(value) => serde_json::Value::from(value),
                        ValueRef::Text(value) => {
                            serde_json::Value::String(String::from_utf8_lossy(value).into_owned())
                        }
                        ValueRef::Blob(value) => {
                            serde_json::Value::String(bytes_to_hex(value))
                        }
                    };
                    values.insert(column_name.clone(), value);
                }
                result_rows.push(serde_json::Value::Object(values));
            }

            return serde_json::to_string(&result_rows).map_err(Into::into);
        }

        let rows_affected = statement.raw_execute()?;
        serde_json::to_string(&serde_json::json!({
            "rows_affected": rows_affected,
        }))
        .map_err(Into::into)
    }
}

impl CapabilityProvider for SqliteCapabilityProvider {
    fn capability_name(&self) -> &'static str {
        "sqlite"
    }
}

#[derive(Debug, Clone, PartialEq)]
enum SqliteParams {
    None,
    Positional(Vec<SqlValue>),
    Named(BTreeMap<String, SqlValue>),
}

fn parse_sqlite_params(params_json: Option<&str>) -> Result<SqliteParams, CapabilityProviderError> {
    let Some(params_json) = params_json else {
        return Ok(SqliteParams::None);
    };
    if params_json.trim().is_empty() {
        return Ok(SqliteParams::None);
    }

    let value: serde_json::Value = serde_json::from_str(params_json)?;
    match value {
        serde_json::Value::Null => Ok(SqliteParams::None),
        serde_json::Value::Array(values) => values
            .into_iter()
            .map(json_to_sql_value)
            .collect::<Result<Vec<_>, _>>()
            .map(SqliteParams::Positional),
        serde_json::Value::Object(values) => values
            .into_iter()
            .map(|(key, value)| Ok((key, json_to_sql_value(value)?)))
            .collect::<Result<BTreeMap<_, _>, CapabilityProviderError>>()
            .map(SqliteParams::Named),
        other => Err(CapabilityProviderError::new(format!(
            "sqlite params JSON must be null, array, or object; got {other}"
        ))),
    }
}

fn bind_statement(
    statement: &mut Statement<'_>,
    params: &SqliteParams,
) -> Result<(), CapabilityProviderError> {
    match params {
        SqliteParams::None => Ok(()),
        SqliteParams::Positional(values) => {
            for (index, value) in values.iter().enumerate() {
                statement.raw_bind_parameter(index + 1, value)?;
            }
            Ok(())
        }
        SqliteParams::Named(values) => {
            for index in 1..=statement.parameter_count() {
                let name = statement.parameter_name(index).ok_or_else(|| {
                    CapabilityProviderError::new(format!(
                        "missing parameter name for SQLite bind index {index}"
                    ))
                })?;
                let key = name
                    .strip_prefix(':')
                    .or_else(|| name.strip_prefix('@'))
                    .or_else(|| name.strip_prefix('$'))
                    .unwrap_or(name);
                let value = values.get(key).ok_or_else(|| {
                    CapabilityProviderError::new(format!(
                        "missing named SQLite parameter '{key}'"
                    ))
                })?;
                statement.raw_bind_parameter(index, value)?;
            }
            Ok(())
        }
    }
}

fn json_to_sql_value(value: serde_json::Value) -> Result<SqlValue, CapabilityProviderError> {
    match value {
        serde_json::Value::Null => Ok(SqlValue::Null),
        serde_json::Value::Bool(value) => Ok(SqlValue::Integer(i64::from(value))),
        serde_json::Value::Number(number) => {
            if let Some(value) = number.as_i64() {
                Ok(SqlValue::Integer(value))
            } else if let Some(value) = number.as_u64() {
                i64::try_from(value)
                    .map(SqlValue::Integer)
                    .map_err(|_| CapabilityProviderError::new("sqlite integer parameter overflow"))
            } else if let Some(value) = number.as_f64() {
                Ok(SqlValue::Real(value))
            } else {
                Err(CapabilityProviderError::new("unsupported sqlite numeric parameter"))
            }
        }
        serde_json::Value::String(value) => Ok(SqlValue::Text(value)),
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
            serde_json::to_string(&value).map(SqlValue::Text).map_err(Into::into)
        }
    }
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(hex_digit(byte >> 4));
        output.push(hex_digit(byte & 0x0f));
    }
    output
}

fn hex_digit(nibble: u8) -> char {
    match nibble {
        0..=9 => (b'0' + nibble) as char,
        10..=15 => (b'a' + (nibble - 10)) as char,
        _ => unreachable!("nibble out of range"),
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::SqliteCapabilityProvider;

    #[test]
    fn sqlite_provider_executes_write_and_read_queries() {
        let path = temp_db_path("sqlite-provider");
        let provider = SqliteCapabilityProvider::new(&path).unwrap();

        provider
            .execute(
                "CREATE TABLE agents (id TEXT PRIMARY KEY, loyalty INTEGER NOT NULL)",
                None,
            )
            .unwrap();
        let insert = provider
            .execute(
                "INSERT INTO agents (id, loyalty) VALUES (:id, :loyalty)",
                Some(r#"{"id":"agent-1","loyalty":91}"#),
            )
            .unwrap();
        assert!(insert.contains(r#""rows_affected":1"#));

        let rows = provider
            .execute(
                "SELECT id, loyalty FROM agents WHERE id = :id",
                Some(r#"{"id":"agent-1"}"#),
            )
            .unwrap();
        assert_eq!(rows, r#"[{"id":"agent-1","loyalty":91}]"#);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn sqlite_provider_supports_positional_parameters() {
        let path = temp_db_path("sqlite-provider-positional");
        let provider = SqliteCapabilityProvider::new(&path).unwrap();

        provider
            .execute(
                "CREATE TABLE weather (location TEXT PRIMARY KEY, condition TEXT NOT NULL)",
                None,
            )
            .unwrap();
        provider
            .execute(
                "INSERT INTO weather (location, condition) VALUES (?, ?)",
                Some(r#"["Trieste","rain"]"#),
            )
            .unwrap();

        let rows = provider
            .execute(
                "SELECT condition FROM weather WHERE location = ?",
                Some(r#"["Trieste"]"#),
            )
            .unwrap();
        assert_eq!(rows, r#"[{"condition":"rain"}]"#);

        let _ = std::fs::remove_file(path);
    }

    fn temp_db_path(prefix: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("pera-{prefix}-{nanos}.sqlite"))
    }
}
