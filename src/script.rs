//! The emitted script, split by how long each statement's effect lasts.
//!
//! A SQLite database file keeps its schema, while pragmas and `TEMP` objects
//! are connection state lost on close, so the file part runs once and the
//! setup and connection parts on every open.

#[cfg(not(feature = "std"))]
use alloc::{string::String, vec::Vec};

use sqlparser::ast::Statement;

use crate::errors::{Error, TranslationRefusal};

/// Why the connection part cannot be produced.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConnectionError {
    /// A replayed statement reads a relation the final schema no longer has.
    #[error("connection replay needs dropped relation `{relation}` at statement {statement}")]
    UnavailableRelation {
        /// The dropped relation.
        relation: String,
        /// The zero-based source statement position.
        statement: usize,
    },
    /// A replayed statement reads a column the final schema no longer has.
    #[error(
        "connection replay needs dropped column `{column}` of `{relation}` at statement {statement}"
    )]
    UnavailableColumn {
        /// The relation that held the column.
        relation: String,
        /// The dropped column.
        column: String,
        /// The zero-based source statement position.
        statement: usize,
    },
    /// Replay cannot preserve the source semantics.
    #[error(transparent)]
    Translation(#[from] TranslationRefusal),
}

/// A translation's statements, in parts run in field order.
///
/// Every connection runs [`setup`](Self::setup). The connection creating the
/// database then runs [`main`](Self::main), and every later connection runs
/// [`connect`](Self::connect) instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Script<S = Statement> {
    /// Connection pragmas, `PRAGMA case_sensitive_like = 1` whenever the
    /// script is not empty, so `LIKE` matches case as PostgreSQL does, and
    /// `PRAGMA foreign_keys = 1` when a table declares a foreign key, since
    /// SQLite enforces one only under that pragma.
    pub setup: Vec<S>,
    /// Once per main database file, in source order.
    pub main: Vec<S>,
    /// The committed `TEMP` objects and rows the source leaves behind,
    /// recreated on a connection to an existing database file.
    pub connection: Result<Vec<S>, ConnectionError>,
}

impl<S> Script<S> {
    /// Whether no part holds a statement.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.setup.is_empty()
            && self.main.is_empty()
            && self.connection.as_ref().is_ok_and(Vec::is_empty)
    }

    /// The statements a connection creating the database runs, in order.
    pub fn iter(&self) -> impl Iterator<Item = &S> {
        self.setup.iter().chain(&self.main)
    }

    /// The statements a connection to an existing database file runs, in
    /// order.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Connection`] when the connection part needs a
    /// relation or column the final schema dropped, or would repeat a write
    /// to the database file.
    pub fn connect(&self) -> Result<impl Iterator<Item = &S>, Error> {
        let connection =
            self.connection.as_ref().map_err(|error| Error::Connection(error.clone()))?;
        Ok(self.setup.iter().chain(connection))
    }

    /// The statements a connection creating the database runs, in order.
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
            connection: self
                .connection
                .map(|statements| statements.into_iter().map(&mut f).collect()),
        }
    }
}
