//! The emitted script, split by how long each statement's effect lasts.
//!
//! A SQLite database file keeps its schema, while pragmas are connection state
//! lost on close, so the file part runs once and the setup part on every open.

#[cfg(not(feature = "std"))]
use alloc::vec::Vec;

use sqlparser::ast::Statement;

/// A translation's statements, in parts run in field order.
///
/// Every connection runs [`setup`](Self::setup), and the connection creating
/// the database then runs [`main`](Self::main).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Script<S = Statement> {
    /// Connection pragmas, `PRAGMA case_sensitive_like = 1` whenever the
    /// script is not empty, so `LIKE` matches case as PostgreSQL does, and
    /// `PRAGMA foreign_keys = 1` when a table declares a foreign key, since
    /// SQLite enforces one only under that pragma.
    pub setup: Vec<S>,
    /// Once per main database file.
    pub main: Vec<S>,
}

impl<S> Script<S> {
    /// Whether no part holds a statement.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.setup.is_empty() && self.main.is_empty()
    }

    /// The statements of every part, in the order a connection creating the
    /// database runs them.
    pub fn iter(&self) -> impl Iterator<Item = &S> {
        self.setup.iter().chain(&self.main)
    }

    /// The statements of every part, in the order a connection creating the
    /// database runs them.
    #[must_use]
    pub fn into_vec(self) -> Vec<S> {
        let mut statements = self.setup;
        statements.extend(self.main);
        statements
    }

    /// Applies `f` to every statement, keeping each in its part.
    #[must_use]
    pub fn map<T>(self, mut f: impl FnMut(S) -> T) -> Script<T> {
        Script {
            setup: self.setup.into_iter().map(&mut f).collect(),
            main: self.main.into_iter().map(&mut f).collect(),
        }
    }
}
