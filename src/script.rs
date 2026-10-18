//! Ordered file initialization and connection-local replay.

#[cfg(not(feature = "std"))]
use alloc::{string::String, vec::Vec};

use sqlparser::ast::Statement;

use crate::{
    errors::{Error, TranslationRefusal},
    initialization::InitializationPlan,
};

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

/// Which newly created database files an initialization runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreshFiles {
    /// The main database file.
    Main,
    /// The attached database file.
    Attached,
    /// Every database file the translation creates.
    Both,
}

impl FreshFiles {
    /// The selection as a mode mask: `1` main, `2` attached, `4` both.
    #[must_use]
    pub fn mask(self) -> u8 {
        match self {
            FreshFiles::Main => 1,
            FreshFiles::Attached => 2,
            FreshFiles::Both => 4,
        }
    }
}

/// Ordered initialization plus the connection state recreated on every open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Script<S = Statement> {
    /// Pragmas applied before initialization or connection replay.
    pub setup: Vec<S>,
    /// Source-ordered definitions, triggers, seed writes and transaction
    /// controls, selected per newly created file.
    pub initialization: InitializationPlan<S>,
    /// Committed connection-local definitions and temporary-storage
    /// initialization.
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

    /// Connection pragmas followed by source-ordered initialization of every
    /// file the translation creates.
    pub fn iter(&self) -> impl Iterator<Item = &S> {
        self.setup.iter().chain(self.initialization.iter())
    }

    /// Connection pragmas followed by initialization of the selected fresh
    /// files, in source order.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Initialization`] when selected statements require
    /// unavailable history or name a file the translation does not create.
    pub fn initialize(&self, files: FreshFiles) -> Result<impl Iterator<Item = &S>, Error> {
        let initialization = self.initialization.select(files)?;
        Ok(self.setup.iter().chain(initialization))
    }

    /// Pragmas and committed connection-local state for an existing database.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Connection`] when replay needs unavailable history or
    /// would repeat persistent table writes.
    pub fn connect(&self) -> Result<impl Iterator<Item = &S>, Error> {
        let connection =
            self.connection.as_ref().map_err(|error| Error::Connection(error.clone()))?;
        Ok(self.setup.iter().chain(connection))
    }

    /// Connection pragmas followed by source-ordered initialization of every
    /// file the translation creates.
    #[must_use]
    pub fn into_vec(self) -> Vec<S> {
        let mut statements = self.setup;
        statements.extend(self.initialization.into_statements());
        statements
    }

    /// Applies `f` to every statement, keeping each in its part.
    #[must_use]
    pub fn map<T>(self, mut f: impl FnMut(S) -> T) -> Script<T> {
        Script {
            setup: self.setup.into_iter().map(&mut f).collect(),
            initialization: self.initialization.map(&mut f),
            connection: self
                .connection
                .map(|statements| statements.into_iter().map(&mut f).collect()),
        }
    }
}
