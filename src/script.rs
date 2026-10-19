//! Ordered file initialization and connection-local replay.

#[cfg(not(feature = "std"))]
use alloc::{string::String, vec::Vec};

use sqlparser::ast::Statement;

use crate::errors::{Error, TranslationRefusal};

/// A dependency unavailable to connection-local replay.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConnectionError {
    /// A source relation is absent from the final persistent schema.
    #[error("connection replay needs dropped relation `{relation}` at statement {statement}")]
    UnavailableRelation {
        /// The required relation.
        relation: String,
        /// The zero-based initializer statement position.
        statement: usize,
    },
    /// A source field is absent from the final persistent schema.
    #[error(
        "connection replay needs dropped column `{column}` of `{relation}` at statement {statement}"
    )]
    UnavailableColumn {
        /// The required relation.
        relation: String,
        /// The required field.
        column: String,
        /// The zero-based initializer statement position.
        statement: usize,
    },
    /// A connection definition cannot preserve its source semantics.
    #[error(transparent)]
    Translation(#[from] TranslationRefusal),
}

/// Ordered initialization plus the connection state recreated on every open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Script<S = Statement> {
    /// Pragmas applied before initialization or connection replay.
    pub setup: Vec<S>,
    /// Source-ordered definitions, triggers, seed writes and transaction
    /// controls.
    pub initialization: Vec<S>,
    /// Final connection-local definitions and temporary-storage initialization.
    pub connection: Result<Vec<S>, ConnectionError>,
}

impl<S> Script<S> {
    /// Whether no part holds a statement.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.setup.is_empty()
            && self.initialization.is_empty()
            && self.connection.as_ref().is_ok_and(Vec::is_empty)
    }

    /// Connection pragmas followed by source-ordered initialization.
    pub fn iter(&self) -> impl Iterator<Item = &S> {
        self.setup.iter().chain(&self.initialization)
    }

    /// Pragmas and final connection-local state for an existing database.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Connection`] when replay needs unavailable history.
    pub fn connect(&self) -> Result<impl Iterator<Item = &S>, Error> {
        let connection =
            self.connection.as_ref().map_err(|error| Error::Connection(error.clone()))?;
        Ok(self.setup.iter().chain(connection))
    }

    /// Connection pragmas followed by source-ordered initialization.
    #[must_use]
    pub fn into_vec(self) -> Vec<S> {
        let mut statements = self.setup;
        statements.extend(self.initialization);
        statements
    }

    /// Applies `f` to every statement, keeping each in its part.
    #[must_use]
    pub fn map<T>(self, mut f: impl FnMut(S) -> T) -> Script<T> {
        Script {
            setup: self.setup.into_iter().map(&mut f).collect(),
            initialization: self.initialization.into_iter().map(&mut f).collect(),
            connection: self
                .connection
                .map(|statements| statements.into_iter().map(&mut f).collect()),
        }
    }
}
