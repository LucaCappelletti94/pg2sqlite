//! Thin wrapper around pg2sqlite's forward translation pipeline.
//!
//! Centralises the call shape so Step 1's translate button, Step 1's
//! debounced auto-translation, and Step 3's PG-dialect query path all
//! invoke pg2sqlite the same way.
//!
//! The wrapper deliberately produces a `TranslationError` instead of
//! leaking pg2sqlite's `Error` enum to the UI. The Step 1 error card
//! displays a category badge + a message; that's all the call sites
//! need.

use pg2sqlite::{
    errors::Error as PgError,
    prelude::{Pg2Sqlite, Pg2SqliteOptions},
};

use crate::state::{ErrorCategory, TranslationError, TranslationStats};

/// Successful translation result.
pub struct TranslationOutput {
    pub sqlite_sql: String,
    pub stats: TranslationStats,
}

/// Translate `pg_sql` to SQLite using `options`. Returns the joined
/// `;\n`-separated SQLite SQL plus per-call stats (statement count +
/// elapsed wall-clock ms).
///
/// `elapsed_ms_fn` is injected so the browser can pass
/// `web_sys::Performance::now`-based timing without coupling this
/// module to wasm-bindgen. `FnMut` so we can call it twice (before
/// and after translation).
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

/// Reverse-translate `sqlite_sql` to PostgreSQL using the schema
/// implied by `pg_schema_sql` (re-parsed every call - the playground
/// keeps PG input + options as the source of truth and rebuilds the
/// schema on demand rather than carrying a `ParserDB` through state).
///
/// Returns the joined `;\n`-separated PG SQL.
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

/// Map a pg2sqlite `Error` to the UI-facing categorisation. The
/// boundaries follow the variant docs in `src/errors.rs` of the main
/// crate.
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
