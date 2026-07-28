//! Thin wrapper around pg2sqlite's forward translation pipeline.

use pg2sqlite::{
    errors::Error as PgError,
    prelude::{Pg2Sqlite, Pg2SqliteOptions},
};

use crate::state::{ErrorCategory, TranslationError, TranslationStats};

pub struct TranslationOutput {
    pub sqlite_sql: String,
    pub stats: TranslationStats,
}

/// `elapsed_ms_fn` is injected so the browser can pass
/// `web_sys::Performance::now`-based timing without coupling to wasm-bindgen.
/// `FnMut` so we can call it twice.
pub fn translate(
    pg_sql: &str,
    options: &Pg2SqliteOptions,
    mut elapsed_ms_fn: impl FnMut() -> f64,
) -> Result<TranslationOutput, TranslationError> {
    let start = elapsed_ms_fn();
    let stmts = Pg2Sqlite::default()
        .sql(pg_sql)
        .and_then(|t| t.translate(options))
        .map_err(classify_error)?;

    let elapsed_ms = elapsed_ms_fn() - start;
    let sqlite_sql = stmts.iter().map(ToString::to_string).collect::<Vec<_>>().join(";\n");

    Ok(TranslationOutput {
        sqlite_sql,
        stats: TranslationStats { statement_count: stmts.len(), elapsed_ms },
    })
}

/// No `translate_statement_against_schema` API exists, so we prepend the
/// schema, translate the combined SQL, and discard the leading statements.
pub fn translate_query(
    pg_query: &str,
    pg_schema_sql: &str,
    options: &Pg2SqliteOptions,
) -> Result<String, TranslationError> {
    let schema_stmts = Pg2Sqlite::default()
        .sql(pg_schema_sql)
        .and_then(|t| t.translate(options))
        .map_err(classify_error)?;
    let combined_sql = format!("{pg_schema_sql};\n{pg_query}");
    let combined_stmts = Pg2Sqlite::default()
        .sql(&combined_sql)
        .and_then(|t| t.translate(options))
        .map_err(classify_error)?;
    let tail = combined_stmts.into_iter().skip(schema_stmts.len()).collect::<Vec<_>>();
    Ok(tail.iter().map(ToString::to_string).collect::<Vec<_>>().join(";\n"))
}

/// Schema is re-parsed every call because the playground keeps the PG input as
/// the source of truth rather than carrying a `ParserDB` through state.
pub fn reverse_translate(
    sqlite_sql: &str,
    pg_schema_sql: &str,
    options: &Pg2SqliteOptions,
) -> Result<String, TranslationError> {
    let translator = Pg2Sqlite::default().sql(pg_schema_sql).map_err(classify_error)?;
    let schema = translator.build_schema().map_err(classify_error)?;
    let stmts = translator.reverse_sql(sqlite_sql, &schema, options).map_err(classify_error)?;
    Ok(stmts.iter().map(ToString::to_string).collect::<Vec<_>>().join(";\n"))
}

fn classify_error(err: PgError) -> TranslationError {
    let category = match &err {
        PgError::ParserError(..) => ErrorCategory::Parser,
        PgError::SchemaError(_) => ErrorCategory::Schema,
        PgError::UnsupportedSQLiteFeature(_)
        | PgError::UnknownPostgresFeature(_)
        | PgError::UnsupportedReverseStatement { .. }
        | PgError::UnsupportedRlsExpressionVariant { .. }
        | PgError::UnsupportedPolicyPattern { .. }
        | PgError::UnsupportedSchemaQualification { .. } => ErrorCategory::Unsupported,
        PgError::SessionVariableMappingNotFound { .. }
        | PgError::RlsAuditTableNameRequired
        | PgError::MissingPrimaryKeyInUpsert { .. } => ErrorCategory::ConfigRequired,
        _ => ErrorCategory::Other,
    };

    TranslationError { category, message: err.to_string() }
}
