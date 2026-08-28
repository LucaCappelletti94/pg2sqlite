//! Submodule defining a set of translation options.

#[cfg(not(feature = "std"))]
#[allow(unused_imports)]
use alloc::{
    borrow::ToOwned,
    boxed::Box,
    format,
    string::{String, ToString},
    vec,
    vec::Vec,
};

use sqlparser::{ast::DataType, dialect::PostgreSqlDialect, parser::Parser, tokenizer::Token};

use crate::errors::{Error, SqlParseError};

/// Enum for defining the representation of UUIDs in `SQLite`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub enum UuidRepresentation {
    /// Represent UUIDs as BLOBs.
    Blob,
    /// Represent UUIDs as TEXT.
    Text,
}

/// Enum for defining the representation of PostgreSQL arrays in `SQLite`.
///
/// `Json` is currently the only representation. The enum exists because the
/// `Option<ArrayRepresentation>` in the options doubles as the enable switch:
/// with none configured, every array construct is refused rather than
/// silently downgraded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub enum ArrayRepresentation {
    /// Store arrays as JSON array text in a `TEXT` column and translate array
    /// operations through the SQLite `json1` extension, which is compiled in
    /// by default since SQLite 3.38.
    ///
    /// The stored form is a JSON array (`[1,2,3]`), not PostgreSQL's own array
    /// output format (`{1,2,3}`), so a pipeline copying rows between the two
    /// databases has to convert values as well as schema.
    Json,
}

/// Enum for defining the version of UUIDs to generate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UuidVersion {
    /// Version 4 UUIDs (random).
    #[default]
    V4,
    /// Version 7 UUIDs (time-ordered).
    V7,
}

/// Enum representing PostgreSQL session variable patterns that can be mapped
/// to SQLite functions.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub enum SessionVariablePattern {
    /// Matches `current_user` in PostgreSQL.
    CurrentUser,
    /// Matches `current_setting('variable_name')` in PostgreSQL.
    CurrentSetting {
        /// The name of the session variable (e.g., "app.user_id").
        name: String,
    },
}

impl core::fmt::Display for SessionVariablePattern {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::CurrentUser => write!(f, "current_user"),
            Self::CurrentSetting { name } => write!(f, "current_setting('{name}')"),
        }
    }
}

/// A mapping from a PostgreSQL session variable pattern to a SQLite function.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub struct SessionVariableMapping {
    /// The PostgreSQL pattern to match.
    pub pg_pattern: SessionVariablePattern,
    /// The SQLite function name to use as replacement.
    pub sqlite_function: String,
    /// The PostgreSQL type the setting holds, when the caller recorded one.
    ///
    /// PostgreSQL's `current_setting` answers text, so a predicate comparing it
    /// against a `uuid` or an `integer` column casts it, and the cast is lost
    /// going to SQLite because the replica's function answers the value
    /// directly. Recording the type here lets the cast be written again on
    /// the way back.
    pub pg_type: Option<String>,
    /// Whether this mapping represents the tolerant `current_setting(name,
    /// true)` form rather than the strict one-argument
    /// `current_setting(name)` form.
    ///
    /// True: the reverse direction emits `current_setting(name, true)`
    /// (missing_ok), which answers NULL for an unset setting instead of
    /// raising. False: the reverse direction emits `current_setting(name)`,
    /// which raises when the setting is unset.
    pub missing_ok: bool,
}

impl SessionVariableMapping {
    /// Creates a new session variable mapping. Defaults to tolerant (missing_ok
    /// = true).
    #[must_use]
    pub fn new(pg_pattern: SessionVariablePattern, sqlite_function: impl Into<String>) -> Self {
        Self {
            pg_pattern,
            sqlite_function: sqlite_function.into(),
            pg_type: None,
            missing_ok: true,
        }
    }

    /// Creates a mapping for `current_user`.
    #[must_use]
    pub fn current_user(sqlite_function: impl Into<String>) -> Self {
        Self::new(SessionVariablePattern::CurrentUser, sqlite_function)
    }

    /// Creates a mapping for `current_setting('name')`.
    #[must_use]
    pub fn current_setting(name: impl Into<String>, sqlite_function: impl Into<String>) -> Self {
        Self::new(SessionVariablePattern::CurrentSetting { name: name.into() }, sqlite_function)
    }

    /// Creates a strict mapping for `current_setting('name')`, which reverses
    /// to `current_setting(name)` with no second argument.
    ///
    /// The strict form raises when the setting is unset. The tolerant form
    /// `current_setting(name, true)` (the default from [`current_setting`])
    /// answers NULL instead of raising.
    ///
    /// [`current_setting`]: SessionVariableMapping::current_setting
    #[must_use]
    pub fn current_setting_strict(
        name: impl Into<String>,
        sqlite_function: impl Into<String>,
    ) -> Self {
        Self { missing_ok: false, ..Self::current_setting(name, sqlite_function) }
    }

    /// Records the PostgreSQL type the setting holds, spelled as PostgreSQL
    /// spells it.
    ///
    /// # Example
    /// ```
    /// use pg2sqlite::prelude::*;
    ///
    /// let mapping =
    ///     SessionVariableMapping::current_setting("app.user_id", "app_user_id").with_pg_type("uuid");
    /// assert_eq!(mapping.pg_type.as_deref(), Some("uuid"));
    /// ```
    #[must_use]
    pub fn with_pg_type(mut self, pg_type: impl Into<String>) -> Self {
        self.pg_type = Some(pg_type.into());
        self
    }

    /// The recorded type as a node, or `None` when the caller recorded none.
    ///
    /// A spelling that leaves input behind is refused rather than truncated,
    /// because `parse_data_type` reads one type and stops: `uuid oops` would
    /// otherwise become `uuid`, and `oops uuid` the custom type `oops`, which
    /// PostgreSQL refuses only once the emitted SQL runs.
    ///
    /// # Errors
    ///
    /// Returns [`Error::SessionVariableTypeUnreadable`] when the recorded
    /// spelling does not parse as a PostgreSQL type, or parses and leaves input
    /// behind.
    ///
    /// # Example
    /// ```
    /// use pg2sqlite::prelude::*;
    /// use sqlparser::ast::{DataType, ExactNumberInfo};
    ///
    /// let mapping = SessionVariableMapping::current_setting("app.rate", "app_rate")
    ///     .with_pg_type("numeric(10,2)");
    /// assert_eq!(
    ///     mapping.pg_type_node()?,
    ///     Some(DataType::Numeric(ExactNumberInfo::PrecisionAndScale(10, 2)))
    /// );
    /// # Ok::<(), pg2sqlite::errors::Error>(())
    /// ```
    pub fn pg_type_node(&self) -> Result<Option<DataType>, Error> {
        let Some(pg_type) = &self.pg_type else {
            return Ok(None);
        };
        let unreadable = |source: Option<SqlParseError>| {
            Error::SessionVariableTypeUnreadable {
                pattern: self.pg_pattern.to_string(),
                pg_type: pg_type.clone(),
                source,
            }
        };
        let mut parser = Parser::new(&PostgreSqlDialect {})
            .try_with_sql(pg_type)
            .map_err(|source| unreadable(Some(SqlParseError::new(pg_type.clone(), source))))?;
        let data_type = parser
            .parse_data_type()
            .map_err(|source| unreadable(Some(SqlParseError::new(pg_type.clone(), source))))?;
        if parser.peek_token().token == Token::EOF {
            Ok(Some(data_type))
        } else {
            Err(unreadable(None))
        }
    }
}
