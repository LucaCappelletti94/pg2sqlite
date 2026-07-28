//! Shared Dioxus signals for the web app.
//!
//! Per-signal so Dioxus's reactivity tracks each piece independently:
//! the SQLite output pane shouldn't invalidate when the user toggles
//! a checkbox in the Advanced options, and vice versa.

use dioxus::prelude::*;
use pg2sqlite::prelude::{
    Pg2SqliteOptions, SessionVariableMapping, TranslationOptions, UuidRepresentation,
};

use crate::{runner::QueryOutcome, samples::SAMPLES};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ErrorCategory {
    Parser,
    Schema,
    Unsupported,
    ConfigRequired,
    Other,
}

impl ErrorCategory {
    pub fn label(self) -> &'static str {
        match self {
            Self::Parser => "Parser",
            Self::Schema => "Schema",
            Self::Unsupported => "Unsupported",
            Self::ConfigRequired => "Config required",
            Self::Other => "Error",
        }
    }
}

#[derive(Clone, PartialEq)]
pub struct TranslationError {
    pub category: ErrorCategory,
    pub message: String,
}

#[derive(Clone, Copy, PartialEq)]
pub struct TranslationStats {
    pub statement_count: usize,
    pub elapsed_ms: f64,
}

/// Mirrors `Pg2SqliteOptions` with plain public fields. The upstream builder
/// can only set fields, never clear them, so we rebuild from scratch each
/// translation.
#[derive(Clone, PartialEq)]
pub struct WebOptions {
    pub uuid_representation: Option<UuidRepresentation>,
    /// Empty means "leave at the crate default" (currently `uuidv7`).
    pub uuid_function_name: String,
    /// Empty means "don't configure", which is fine for non-RLS schemas.
    pub rls_audit_table_name: String,
    pub session_variables: Vec<SessionVariableMapping>,
}

impl Default for WebOptions {
    fn default() -> Self {
        Self {
            uuid_representation: Some(UuidRepresentation::Blob),
            uuid_function_name: String::new(),
            rls_audit_table_name: String::new(),
            session_variables: Vec::new(),
        }
    }
}

impl WebOptions {
    pub fn to_options(&self) -> Pg2SqliteOptions {
        let mut opts = Pg2SqliteOptions::default().with_sqlitegis_enabled();
        if let Some(rep) = self.uuid_representation {
            opts = opts.with_uuid_representation(rep);
        }
        if !self.uuid_function_name.is_empty() {
            opts = opts.with_uuid_function_name(self.uuid_function_name.clone());
        }
        if !self.rls_audit_table_name.is_empty() {
            opts = opts.with_rls_audit_table_name(self.rls_audit_table_name.clone());
        }
        for mapping in &self.session_variables {
            opts = opts.with_session_variable(mapping.clone());
        }
        opts
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum QueryDialect {
    #[default]
    Postgres,
    Sqlite,
}

#[derive(Clone, PartialEq)]
pub struct QueryDisplay {
    pub effective_sql: String,
    pub outcome: QueryOutcome,
}

pub type ReverseOutcome = Result<String, TranslationError>;

#[derive(Clone, Copy)]
pub struct AppState {
    pub pg_input: Signal<String>,
    /// Kept alongside `pg_input` so the pane can reorder and remove files,
    /// rebuilding `pg_input` from this list.
    pub input_files: Signal<Vec<(String, String)>>,
    pub sqlite_output: Signal<Option<String>>,
    pub translation_error: Signal<Option<TranslationError>>,
    pub apply_error: Signal<Option<String>>,
    pub apply_ok: Signal<bool>,
    pub stats: Signal<Option<TranslationStats>>,
    pub options: Signal<WebOptions>,
    pub query_input: Signal<String>,
    pub query_dialect: Signal<QueryDialect>,
    pub query_result: Signal<Option<QueryDisplay>>,
    pub reverse_input: Signal<String>,
    pub reverse_output: Signal<Option<ReverseOutcome>>,
}

impl AppState {
    /// Construct initial state. Seeded with the first sample so a first visitor
    /// lands on a real translation.
    pub fn new() -> Self {
        let seed = &SAMPLES[0];
        let mut options = WebOptions::default();
        (seed.apply_options)(&mut options);
        Self {
            pg_input: Signal::new(seed.sql.to_string()),
            input_files: Signal::new(Vec::new()),
            sqlite_output: Signal::new(None),
            translation_error: Signal::new(None),
            apply_error: Signal::new(None),
            apply_ok: Signal::new(false),
            stats: Signal::new(None),
            options: Signal::new(options),
            query_input: Signal::new(String::new()),
            query_dialect: Signal::new(QueryDialect::default()),
            query_result: Signal::new(None),
            reverse_input: Signal::new(String::new()),
            reverse_output: Signal::new(None),
        }
    }
}
