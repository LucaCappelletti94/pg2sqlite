//! Shared Dioxus signals for the web app.
//!
//! Per-signal so Dioxus's reactivity tracks each piece independently:
//! the SQLite output pane shouldn't invalidate when the user toggles
//! a checkbox in the Advanced options, and vice versa.
//!
//! Section visibility is derived rather than encoded in a state
//! enum:
//!
//! - `sqlite_output.is_some()` -> the side-by-side SQLite pane has something to
//!   show; the reverse-translation panel becomes available.
//! - `apply_ok` -> the in-memory SQLite has the translated schema applied; the
//!   query panel becomes available.

use dioxus::prelude::*;
use pg2sqlite::prelude::{
    Pg2SqliteOptions, SessionVariableMapping, TranslationOptions, UuidRepresentation,
};

use crate::runner::QueryOutcome;

/// Categorises a translation `Error` for the UI badge. Mirrors the
/// `pg2sqlite::errors::Error` variant boundaries so users see the
/// same vocabulary the crate's docs use.
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

/// A surface-level translation error rendered in the inline error
/// card under the editors.
#[derive(Clone, PartialEq)]
pub struct TranslationError {
    pub category: ErrorCategory,
    pub message: String,
}

/// Per-translation stats shown alongside the SQLite pane.
#[derive(Clone, Copy, PartialEq)]
pub struct TranslationStats {
    pub statement_count: usize,
    pub elapsed_ms: f64,
}

/// Local mirror of `Pg2SqliteOptions`'s knobs, with public fields so
/// the form components can mutate them directly. We can't carry an
/// authoritative `Pg2SqliteOptions` through state because the
/// upstream type only exposes builder methods that *set* fields -
/// there's no way to clear `geolite` or the UUID representation once
/// set. By owning a plain struct here and rebuilding
/// `Pg2SqliteOptions` from scratch every translation, the UI gets
/// unrestricted edit semantics.
#[derive(Clone, Default, PartialEq)]
pub struct WebOptions {
    pub uuid_representation: Option<UuidRepresentation>,
    /// Empty means "leave at the crate default" (currently `uuidv7`).
    pub uuid_function_name: String,
    pub geolite_enabled: bool,
    /// Empty means "don't configure", which is fine for non-RLS schemas.
    pub rls_audit_table_name: String,
    pub session_variables: Vec<SessionVariableMapping>,
}

impl WebOptions {
    /// Build a fresh `Pg2SqliteOptions` from the local state. Called
    /// at every translation. Skips knobs whose value is the "leave
    /// alone" sentinel so they keep their crate-default behaviour.
    pub fn to_options(&self) -> Pg2SqliteOptions {
        let mut opts = Pg2SqliteOptions::default();
        if let Some(rep) = self.uuid_representation {
            opts = opts.with_uuid_representation(rep);
        }
        if !self.uuid_function_name.is_empty() {
            opts = opts.with_uuid_function_name(self.uuid_function_name.clone());
        }
        if self.geolite_enabled {
            opts = opts.with_geolite_enabled();
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

/// Which SQL dialect the user is typing into the query box. PG goes
/// through `pg2sqlite::Pg2Sqlite::translate` first; SQLite runs as-is.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum QueryDialect {
    #[default]
    Postgres,
    Sqlite,
}

/// Last query's "Translated as: ..." annotation (only populated for
/// the PG dialect path).
#[derive(Clone, PartialEq)]
pub struct QueryDisplay {
    /// The SQLite SQL that actually ran. Same as the user input for
    /// the SQLite dialect; rewritten by pg2sqlite for the PG dialect.
    pub effective_sql: String,
    pub outcome: QueryOutcome,
}

/// Result of the most recent reverse translation (SQLite -> PG).
/// `Ok` carries the joined PG SQL output; `Err` carries the same
/// translation-error envelope the inline error card uses, so the
/// reverse panel can render the matching category badge.
pub type ReverseOutcome = Result<String, TranslationError>;

/// Bag of signals owned at the root `App` component and passed via
/// `use_context_provider` so any descendant can `use_context` them
/// without prop-drilling. Cloning the struct is cheap because each
/// `Signal<T>` is itself a copy-friendly handle.
#[derive(Clone, Copy)]
pub struct AppState {
    pub pg_input: Signal<String>,
    pub sqlite_output: Signal<Option<String>>,
    pub translation_error: Signal<Option<TranslationError>>,
    pub apply_error: Signal<Option<String>>,
    /// `true` when the most recent forward translation also applied
    /// cleanly to the in-memory SQLite. Gates the query panel.
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
    /// Construct the initial state. Called once at `App` mount.
    pub fn new() -> Self {
        Self {
            pg_input: Signal::new(String::new()),
            sqlite_output: Signal::new(None),
            translation_error: Signal::new(None),
            apply_error: Signal::new(None),
            apply_ok: Signal::new(false),
            stats: Signal::new(None),
            options: Signal::new(WebOptions::default()),
            query_input: Signal::new(String::new()),
            query_dialect: Signal::new(QueryDialect::default()),
            query_result: Signal::new(None),
            reverse_input: Signal::new(String::new()),
            reverse_output: Signal::new(None),
        }
    }
}
