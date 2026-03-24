use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::anyhow;
use pera_canonical::{CanonicalInterface, CatalogSkill};
use pera_core::{ActionSkillRef, StoreError};
use rusqlite::types::{Value as SqlValue, ValueRef};
use rusqlite::{Connection, Statement};
use tracing::trace;
use wasmtime::component::Linker;

use super::{CapabilityProvider, CapabilityProviderError};
use crate::catalog::{InvocationErrorSource, InvocationEventSource, WasmHostState};

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

    pub fn execute_batch(&self, sql: &str) -> Result<(), CapabilityProviderError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| CapabilityProviderError::new("sqlite connection mutex is poisoned"))?;
        connection.execute_batch(sql)?;
        Ok(())
    }
}

impl CapabilityProvider for SqliteCapabilityProvider {
    fn capability_name(&self) -> &'static str {
        "sqlite"
    }
}

pub(crate) fn matches_import(import: &CanonicalInterface) -> bool {
    import.functions.iter().any(|function| function.name == "execute")
        && import.name.contains("sqlite")
}

pub(crate) fn resolve_database_path(
    root: &Path,
    skill_ref: &ActionSkillRef,
    skill: &CatalogSkill,
) -> Result<PathBuf, StoreError> {
    let sqlite_databases = skill
        .databases
        .iter()
        .filter(|database| database.engine == "sqlite")
        .collect::<Vec<_>>();
    let database = match sqlite_databases.as_slice() {
        [database] => *database,
        [] => {
            return Err(StoreError::new(format!(
                "skill '{}' does not define a sqlite database",
                skill_ref.skill_name
            )))
        }
        _ => {
            return Err(StoreError::new(format!(
                "skill '{}' defines multiple sqlite databases; capability mapping is ambiguous",
                skill_ref.skill_name
            )))
        }
    };
    let skill_version = skill_ref
        .skill_version
        .as_ref()
        .map(|version| version.as_str())
        .ok_or_else(|| {
            StoreError::new(format!(
                "skill '{}' is missing a version",
                skill_ref.skill_name
            ))
        })?;
    let profile_name = skill_ref.profile_name.as_deref().ok_or_else(|| {
        StoreError::new(format!(
            "skill '{}' is missing a profile name",
            skill_ref.skill_name
        ))
    })?;

    Ok(root
        .join("state")
        .join("skills")
        .join(&skill_ref.skill_name)
        .join(skill_version)
        .join(profile_name)
        .join("databases")
        .join(format!("{}.sqlite", database.name)))
}

pub(crate) fn build_provider(
    database_path: PathBuf,
) -> Result<SqliteCapabilityProvider, StoreError> {
    SqliteCapabilityProvider::new(database_path).map_err(|error| StoreError::new(error.to_string()))
}

impl SqliteCapabilityProvider {
    pub(crate) fn link_import(
        self: Arc<Self>,
        linker: &mut Linker<WasmHostState>,
        import: &CanonicalInterface,
    ) -> Result<(), StoreError> {
        linker
            .root()
            .instance(&import.name)
            .and_then(|mut instance| {
                let sqlite = Arc::clone(&self);
                let import_name = import.name.clone();
                instance.func_wrap(
                    "execute",
                    move |mut store,
                          (sql, params_json): (String, Option<String>)|
                          -> Result<(String,), anyhow::Error> {
                        store.data_mut().record_event(
                            InvocationEventSource::Provider {
                                name: import_name.clone(),
                                operation: "execute".to_owned(),
                            },
                            format!(
                                "db={} sql={:?} params_json={:?}",
                                sqlite.database_path().display(),
                                sql,
                                params_json
                            ),
                        );
                        trace!(
                            import = %import_name,
                            db = %sqlite.database_path().display(),
                            sql = ?sql,
                            params_json = ?params_json,
                            "sqlite import call",
                        );
                        let result = sqlite.execute(&sql, params_json.as_deref()).map_err(|error| {
                            store.data_mut().fail(
                                InvocationErrorSource::Provider {
                                    name: import_name.clone(),
                                    operation: "execute".to_owned(),
                                },
                                error.to_string(),
                            );
                            tracing::error!(
                                import = %import_name,
                                db = %sqlite.database_path().display(),
                                sql = ?sql,
                                params_json = ?params_json,
                                error = %error,
                                "sqlite import error",
                            );
                            anyhow!(error.to_string())
                        })?;
                        trace!(
                            import = %import_name,
                            db = %sqlite.database_path().display(),
                            result_json = %result,
                            "sqlite import ok",
                        );
                        Ok((result,))
                    },
                )
            })
            .map_err(|error| StoreError::new(error.to_string()))
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

    #[test]
    fn sqlite_provider_executes_batches() {
        let path = temp_db_path("sqlite-provider-batch");
        let provider = SqliteCapabilityProvider::new(&path).unwrap();

        provider
            .execute_batch(
                r#"
                CREATE TABLE reports (id TEXT PRIMARY KEY, status TEXT NOT NULL);
                INSERT INTO reports (id, status) VALUES ('report-1', 'ready');
                "#,
            )
            .unwrap();

        let rows = provider
            .execute("SELECT status FROM reports WHERE id = ?", Some(r#"["report-1"]"#))
            .unwrap();
        assert_eq!(rows, r#"[{"status":"ready"}]"#);

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
