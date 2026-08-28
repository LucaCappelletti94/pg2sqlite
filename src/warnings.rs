//! Warnings emitted during translation when constructs have no SQLite
//! equivalent and are dropped or downgraded.
//!
//! Each translation call owns its warning collector and passes it explicitly
//! through every translator. The plain `translate` API discards the collected
//! warnings after the same translation path completes.
//!
//! # When a warning is the right answer
//!
//! A warning is permitted ONLY when the drop provably cannot affect a query
//! result: server administration, publish and subscribe, access control,
//! planner hints. If the reason cannot be stated in one sentence, the construct
//! is a hard error instead, which is the default. A construct whose effect the
//! pipeline realises elsewhere is neither: it emits nothing and warns nothing,
//! and it belongs to the closed list documented in
//! `impls::translator_impls::statement`, which also carries the per-statement
//! classification. `tests/test_no_statement_is_silently_dropped.rs` enforces
//! the whole policy.
//!
//! Do not add a fourth mechanism. There are three outcomes, plus that closed
//! list.

#[cfg(not(feature = "std"))]
#[allow(unused_imports)]
use alloc::{string::String, vec::Vec};

use sqlparser::ast::Statement;

/// A warning emitted during translation.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum TranslationWarning {
    /// A PostgreSQL construct has no SQLite equivalent and was dropped.
    ///
    /// Fields are owned so a warning can name the object it concerns. A
    /// `&'static str` could only ever name the construct kind, which left the
    /// reader to find the affected table, column, or index themselves.
    LossyDrop {
        /// Short identifier for the construct (e.g. `"LISTEN"`).
        construct: String,
        /// Human-readable reason the construct was dropped.
        reason: String,
    },
    /// A table has row level security enabled but no policy grants read access,
    /// so its translated view denies every row.
    ///
    /// PostgreSQL behaves the same way, so the view is correct. This is
    /// reported because it is usually an unfinished migration, and because
    /// no runtime validation monitor is emitted for such a table: the
    /// monitor asks whether a backing row is visible through the view,
    /// which here is always no, so it would flag every write without
    /// distinguishing anything.
    RlsDeniesEveryRow {
        /// The table whose view denies every row.
        table: String,
    },
    /// A construct translated to something that keeps less than it did.
    ///
    /// Distinct from [`TranslationWarning::LossyDrop`], where the construct
    /// disappears. Here it is emitted, and only part of what it meant survives:
    /// `CHAR(3)` becomes `TEXT`, which stores what it is given rather than
    /// padding it to three characters.
    ///
    /// Fields are owned rather than `&'static str` because the location and
    /// the two type names are read from the input.
    LossyDowngrade {
        /// Short identifier for the construct, such as `"CHAR"`.
        construct: String,
        /// What the source declared, such as `"CHAR(3)"`.
        from: String,
        /// What was emitted in its place, such as `"TEXT"`.
        to: String,
        /// Where it was declared. Currently the column name.
        location: String,
        /// What the emitted form no longer does.
        reason: String,
    },
}

pub(crate) type WarningSink<'a> = &'a mut dyn FnMut(TranslationWarning);

impl core::fmt::Display for TranslationWarning {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::LossyDrop { construct, reason } => {
                write!(f, "dropped {construct}: {reason}")
            }
            Self::RlsDeniesEveryRow { table } => {
                write!(
                    f,
                    "row level security on {table} has no read policy, so its view denies every row"
                )
            }
            Self::LossyDowngrade { construct, from, to, location, reason } => {
                write!(f, "downgraded {construct} at {location} from {from} to {to}: {reason}")
            }
        }
    }
}

/// Combined output of [`Pg2Sqlite::translate_with_report`]: the translated
/// statements plus any warnings collected during translation.
///
/// [`Pg2Sqlite::translate_with_report`]: crate::pg2sqlite::Pg2Sqlite::translate_with_report
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranslationReport {
    /// Translated SQLite statements. Equivalent to the `Vec<Statement>`
    /// returned by [`Pg2Sqlite::translate`].
    ///
    /// [`Pg2Sqlite::translate`]: crate::pg2sqlite::Pg2Sqlite::translate
    pub statements: Vec<Statement>,
    /// Warnings collected during translation. Empty when nothing was
    /// dropped or downgraded.
    pub warnings: Vec<TranslationWarning>,
}
