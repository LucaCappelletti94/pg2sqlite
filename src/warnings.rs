//! Warnings emitted during translation when constructs have no SQLite
//! equivalent and are dropped or downgraded.
//!
//! Translators push warnings through a thread-local collector installed
//! by `Pg2Sqlite::translate_with_report`. The plain `translate` API
//! ignores them. The collector is std-only, so under no_std features warnings
//! are silently discarded, there being no allocator to back the global.
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
#[cfg(feature = "std")]
use std::cell::RefCell;

use sqlparser::ast::Statement;

/// A warning emitted during translation.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum TranslationWarning {
    /// A PostgreSQL construct has no SQLite equivalent and was dropped.
    LossyDrop {
        /// Short identifier for the construct (e.g. `"LISTEN"`).
        construct: &'static str,
        /// Human-readable reason the construct was dropped.
        reason: &'static str,
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
}

/// Combined output of [`Pg2Sqlite::translate_with_report`]: the translated
/// statements plus any warnings collected during translation.
///
/// [`Pg2Sqlite::translate_with_report`]: crate::pg2sqlite::Pg2Sqlite::translate_with_report
#[derive(Debug)]
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

#[cfg(feature = "std")]
thread_local! {
    static COLLECTOR: RefCell<Option<Vec<TranslationWarning>>> = const { RefCell::new(None) };
}

/// Push a warning onto the currently-installed collector. No-op when no
/// collector is installed (the plain `translate` API path).
pub(crate) fn emit(warning: TranslationWarning) {
    #[cfg(feature = "std")]
    COLLECTOR.with(|cell| {
        if let Some(vec) = cell.borrow_mut().as_mut() {
            vec.push(warning);
        }
    });
    #[cfg(not(feature = "std"))]
    let _ = warning;
}

/// RAII scope guard that installs a warning collector for the duration
/// of one `translate_with_report` call. The previous collector (if any)
/// is restored on drop so nested calls do not clobber an outer scope.
pub(crate) struct CollectorScope {
    #[cfg(feature = "std")]
    previous: Option<Vec<TranslationWarning>>,
}

impl CollectorScope {
    /// Install a fresh empty collector, saving the previous one.
    pub fn install() -> Self {
        #[cfg(feature = "std")]
        let previous = COLLECTOR.with(|c| c.borrow_mut().replace(Vec::new()));
        Self {
            #[cfg(feature = "std")]
            previous,
        }
    }

    /// Take the collected warnings. Dropping `self` after this returns
    /// runs the `Drop` impl, which restores any previous collector.
    pub fn take(self) -> Vec<TranslationWarning> {
        let collected;
        #[cfg(feature = "std")]
        {
            collected = COLLECTOR.with(|c| c.borrow_mut().take().unwrap_or_default());
        }
        #[cfg(not(feature = "std"))]
        {
            collected = Vec::new();
        }
        drop(self);
        collected
    }
}

#[cfg(feature = "std")]
impl Drop for CollectorScope {
    fn drop(&mut self) {
        COLLECTOR.with(|cell| *cell.borrow_mut() = self.previous.take());
    }
}
