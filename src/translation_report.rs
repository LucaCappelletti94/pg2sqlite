//! Types describing translation results with non-fatal warnings.

use sqlparser::ast::Statement;

/// Detailed result of a PostgreSQL -> SQLite translation run.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TranslationReport {
    /// Translated SQLite statements.
    pub statements: Vec<Statement>,
    /// Non-fatal warnings emitted while translating.
    pub warnings: Vec<TranslationWarning>,
}

/// Warning emitted during translation when an input cannot be fully preserved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranslationWarning {
    /// A PostgreSQL statement variant is unsupported and was skipped.
    UnsupportedStatement {
        /// Statement variant name (for example `AlterTable`).
        statement_variant: String,
        /// Original SQL text for the skipped statement.
        sql: String,
    },
    /// A role-filtered object was skipped for the configured session role.
    SkippedObjectForRole {
        /// Object/statement kind (for example `CREATE INDEX`).
        object_kind: String,
        /// Target object name (for example table name).
        object_name: String,
        /// Human-readable reason the object was skipped.
        reason: String,
    },
    /// A trigger function was missing and the trigger was skipped in non-strict
    /// mode.
    MissingTriggerBody {
        /// Trigger name.
        trigger_name: String,
        /// Referenced trigger function name.
        function_name: String,
    },
}
