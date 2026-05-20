//! Submodule defining the main translator struct.

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
#[cfg(feature = "std")]
use std::path::PathBuf;

#[cfg(feature = "std")]
use git2::Repository;
use sql_traits::structs::ParserDB;
use sqlparser::ast::{IndexType, Statement};
#[cfg(feature = "std")]
use tempfile::TempDir;

use crate::{
    impls::{
        object_name::last_ident_value_or_display,
        translator_impls::{postgis, rls::generate_rls_audit_table},
    },
    options::Pg2SqliteOptions,
    prelude::{ReverseTranslator, Translator},
    traits::TranslationOptions,
};

/// Pre-walks the input statements for GiST indexes over `geometry`/`geography`
/// columns and registers each `(table, column)` pair in the options' internal
/// spatial-index catalog. Used by `Pg2Sqlite::translate_internal` to drive
/// later query-time predicate rewriting through the rtree shadow.
///
/// Classification errors are deliberately swallowed here: the per-statement
/// translation in the main loop will re-run the same classifier and surface
/// the error with full context.
fn populate_spatial_index_catalog(
    statements: &[Statement],
    schema: &ParserDB,
    options: &mut Pg2SqliteOptions,
) {
    for stmt in statements {
        let Statement::CreateIndex(create_index) = stmt else {
            continue;
        };
        if !matches!(create_index.using, Some(IndexType::GiST)) {
            continue;
        }
        let Ok(Some(spatial_columns)) =
            postgis::classify_gist_spatial_columns(create_index, schema)
        else {
            continue;
        };
        let table_name = last_ident_value_or_display(&create_index.table_name);
        for col in spatial_columns {
            options.add_spatial_index(&table_name, &col);
        }
    }
}

#[derive(Debug, Clone, Default)]
/// Struct to translate between a `PostgreSQL` entry and a `SQLite` entry.
pub struct Pg2Sqlite {
    /// The set of `PostgreSQL` statements to be translated.
    pg_statements: Vec<Statement>,
}

impl Pg2Sqlite {
    fn normalize_statements(statements: &[Statement]) -> Vec<Statement> {
        statements.to_vec()
    }

    fn schema_statements_for_translation(statements: &[Statement]) -> Vec<Statement> {
        statements
            .iter()
            .filter(|statement| {
                !matches!(
                    statement,
                    Statement::CreateIndex(_)
                        | Statement::CreateTrigger(_)
                        | Statement::Drop { .. }
                        | Statement::DropTrigger(_)
                )
            })
            .cloned()
            .collect()
    }

    #[must_use]
    /// Adds a new SQL statement to the set of `PostgreSQL` statements to be
    /// translated.
    ///
    /// # Example
    ///
    /// ```
    /// use pg2sqlite::pg2sqlite::Pg2Sqlite;
    /// use sqlparser::{dialect::PostgreSqlDialect, parser::Parser};
    ///
    /// let sql = "CREATE TABLE t (a INT);";
    /// let statement = Parser::parse_sql(&PostgreSqlDialect {}, sql).unwrap().pop().unwrap();
    /// let translator = Pg2Sqlite::default().statement(statement);
    /// ```
    pub fn statement(mut self, statement: Statement) -> Self {
        self.pg_statements.push(statement);
        self
    }

    /// Adds a new SQL statement to be parsed and added to the set of
    /// `PostgreSQL` statements to be translated.
    ///
    /// # Arguments
    ///
    /// * `sql` - The SQL statement to be parsed and added to the set of
    ///   `PostgreSQL` statements to be translated.
    ///
    /// # Returns
    ///
    /// A Result containing the updated `Pg2Sqlite` struct or an error if the
    /// SQL statement could not be parsed.
    ///
    /// # Errors
    ///
    /// * If the SQL statement could not be parsed.
    ///
    /// # Example
    ///
    /// ```
    /// # use pg2sqlite::pg2sqlite::Pg2Sqlite;
    /// let translator = Pg2Sqlite::default().sql("CREATE TABLE t (a INT);").unwrap();
    /// ```
    pub fn sql(mut self, sql: &str) -> Result<Self, crate::errors::Error> {
        let stmt =
            sqlparser::parser::Parser::parse_sql(&sqlparser::dialect::PostgreSqlDialect {}, sql)
                .map_err(|e| crate::errors::Error::ParserError(sql.to_owned(), e))?;
        for statement in stmt {
            self = self.statement(statement);
        }
        Ok(self)
    }

    /// Adds a new path with an SQL file to be parsed and added to the set of
    /// `PostgreSQL` statements to be translated.
    ///
    /// Only available with the `std` feature.
    ///
    /// # Arguments
    ///
    /// * `path` - The path to the SQL file to be parsed and added to the set of
    ///   `PostgreSQL` statements to be translated.
    ///
    /// # Returns
    ///
    /// A Result containing the updated `Pg2Sqlite` struct or an error if the
    /// SQL file could not be read or parsed.
    ///
    /// # Errors
    ///
    /// * If the SQL file could not be read.
    /// * If the SQL file could not be parsed.
    #[cfg(feature = "std")]
    pub fn file<P: AsRef<std::path::Path>>(self, path: P) -> Result<Self, crate::errors::Error> {
        let path = path.as_ref();
        let content = std::fs::read_to_string(path)?;
        self.sql(&content)
    }

    /// Adds all of the `up.sql` migrations found under the given directory to
    /// the set of `PostgreSQL` statements to be translated.
    ///
    /// Only available with the `std` feature.
    ///
    /// # Arguments
    ///
    /// * `path` - The path to the directory containing the `up.sql` migrations.
    ///
    /// # Returns
    ///
    /// A Result containing the updated `Pg2Sqlite` struct or an error if the
    /// SQL files could not be read or parsed.
    ///
    /// # Errors
    ///
    /// * If the SQL files could not be read.
    /// * If the SQL files could not be parsed.
    #[cfg(feature = "std")]
    pub fn ups<P: AsRef<std::path::Path>>(directory: P) -> Result<Self, crate::errors::Error> {
        let up_sql_paths = Self::sorted_up_sql_paths(directory.as_ref())?;
        Self::from_migration_paths(up_sql_paths)
    }

    /// Adds all of the `up.sql` migrations found under the given directory to
    /// the set of `PostgreSQL` statements to be translated, stopping at (and
    /// including) the specified migration.
    ///
    /// Only available with the `std` feature.
    ///
    /// # Arguments
    ///
    /// * `directory` - The path to the directory containing the `up.sql`
    ///   migrations.
    /// * `stop_at` - The path to the migration file where processing should
    ///   stop (inclusive).
    ///
    /// # Returns
    ///
    /// A Result containing the updated `Pg2Sqlite` struct or an error if the
    /// SQL files could not be read or parsed.
    ///
    /// # Errors
    ///
    /// * If the SQL files could not be read.
    /// * If the SQL files could not be parsed.
    #[cfg(feature = "std")]
    pub fn ups_until<P: AsRef<std::path::Path>>(
        directory: P,
        stop_at: P,
    ) -> Result<Self, crate::errors::Error> {
        let up_sql_paths = Self::sorted_up_sql_paths(directory.as_ref())?;
        let stop_index = Self::find_stop_migration_index(&up_sql_paths, stop_at.as_ref())?;
        Self::from_migration_paths(up_sql_paths.iter().take(stop_index + 1))
    }

    #[cfg(feature = "std")]
    fn sorted_up_sql_paths(
        directory: &std::path::Path,
    ) -> Result<Vec<PathBuf>, crate::errors::Error> {
        let mut up_sql_paths = Vec::new();
        Self::collect_up_sql_paths(directory, &mut up_sql_paths)?;
        up_sql_paths.sort();
        Ok(up_sql_paths)
    }

    #[cfg(feature = "std")]
    fn find_stop_migration_index(
        up_sql_paths: &[PathBuf],
        stop_at: &std::path::Path,
    ) -> Result<usize, crate::errors::Error> {
        let stop_at = std::fs::canonicalize(stop_at)?;

        for (index, path) in up_sql_paths.iter().enumerate() {
            if std::fs::canonicalize(path)? == stop_at {
                return Ok(index);
            }
        }

        Err(crate::errors::Error::MigrationNotFound { path: stop_at.display().to_string() })
    }

    #[cfg(feature = "std")]
    fn from_migration_paths<I, P>(paths: I) -> Result<Self, crate::errors::Error>
    where
        I: IntoIterator<Item = P>,
        P: AsRef<std::path::Path>,
    {
        let mut translator = Self::default();
        for path in paths {
            translator = translator.file(path)?;
        }
        Ok(translator)
    }

    #[cfg(feature = "std")]
    fn collect_up_sql_paths(
        directory: &std::path::Path,
        paths: &mut Vec<PathBuf>,
    ) -> Result<(), crate::errors::Error> {
        // We iterate recursively over the migrations directory. Symlinks
        // are skipped so a directory-symlink loop (or a copy alias) does
        // not surface the same `up.sql` twice, which sql-traits now
        // rejects with TableLookupConflict during schema build.
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_symlink() {
                continue;
            }
            let path = entry.path();
            if file_type.is_file() {
                if let Some(file_name) = path.file_name()
                    && file_name == "up.sql"
                {
                    paths.push(path);
                }
            } else if file_type.is_dir() {
                Self::collect_up_sql_paths(&path, paths)?;
            }
        }
        Ok(())
    }

    /// Adds all of the `up.sql` migrations found in the provided git repository
    /// to the set of `PostgreSQL` statements to be translated.
    ///
    /// Only available with the `std` feature.
    ///
    /// # Arguments
    ///
    /// * `url` - The URL of the git repository containing the `up.sql`
    ///   migrations.
    ///
    /// # Returns
    ///
    /// A Result containing the updated `Pg2Sqlite` struct or an error if the
    /// repository could not be cloned or the SQL files could not be read or
    /// parsed.
    ///
    /// # Errors
    ///
    /// * If the git repository could not be cloned.
    /// * If the SQL files could not be read.
    /// * If the SQL files could not be parsed.
    #[cfg(feature = "std")]
    pub fn from_git(url: &str) -> Result<Self, crate::errors::Error> {
        let temp_dir = TempDir::new()?;
        Repository::clone(url, temp_dir.path())
            .map_err(|e| crate::errors::Error::GitError(e.to_string()))?;
        Self::ups(temp_dir.path())
    }

    fn translate_internal(
        self,
        options: &Pg2SqliteOptions,
    ) -> Result<Vec<Statement>, crate::errors::Error> {
        use sql_traits::traits::{DatabaseLike, TableLike};

        let normalized_statements = Self::normalize_statements(&self.pg_statements);
        let schema_statements = Self::schema_statements_for_translation(&normalized_statements);
        let schema = ParserDB::from_statements(schema_statements, "translation_db".to_owned())?;

        // Pre-walk for spatial-index DDL so that the same translation unit's
        // SELECTs can rewrite `ST_*` predicates over indexed columns through
        // the rtree shadow. Only fires when geolite translation is enabled;
        // skips errors (an unsupported GiST will surface its own error when
        // the statement itself is translated below).
        let mut options = options.clone();
        if options.is_geolite_enabled() {
            populate_spatial_index_catalog(&normalized_statements, &schema, &mut options);
        }
        let options = options;

        let mut result: Vec<Statement> = normalized_statements
            .iter()
            .map(|statement| statement.translate(&schema, &options))
            .collect::<Result<Vec<Vec<Statement>>, crate::errors::Error>>()?
            .into_iter()
            .flatten()
            .collect();

        // If any table has RLS enabled and audit table name is configured,
        // prepend the audit table creation statement
        let has_rls_tables = schema.tables().any(|table| table.has_row_level_security(&schema));

        if has_rls_tables && let Some(audit_table_name) = options.get_rls_audit_table_name() {
            let audit_table_stmt = generate_rls_audit_table(audit_table_name)?;
            result.insert(0, audit_table_stmt);
        }

        Ok(result)
    }

    /// Translates the set of `PostgreSQL` statements to `SQLite` statements.
    ///
    /// # Returns
    ///
    /// * A Result containing the set of `SQLite` statements or an error if the
    ///   translation could not be performed.
    ///
    /// # Errors
    ///
    /// * If the translation could not be performed.
    ///
    /// # Panics
    ///
    /// * If the progress bar could not be created.
    ///
    /// # Example
    ///
    /// ```
    /// # use pg2sqlite::pg2sqlite::Pg2Sqlite;
    /// # use pg2sqlite::options::Pg2SqliteOptions;
    /// let translator = Pg2Sqlite::default().sql("CREATE TABLE t (a INT);").unwrap();
    /// let sqlite_statements = translator.translate(&Pg2SqliteOptions::default()).unwrap();
    /// assert!(!sqlite_statements.is_empty());
    /// ```
    pub fn translate(
        self,
        options: &Pg2SqliteOptions,
    ) -> Result<Vec<Statement>, crate::errors::Error> {
        self.translate_internal(options)
    }

    /// Convenience method: translates to a `Vec<String>` of SQL strings.
    ///
    /// Equivalent to `translate()` followed by mapping each statement to its
    /// `to_string()` representation. Useful when you don't need the AST.
    ///
    /// # Errors
    ///
    /// * If parsing or translation fails.
    pub fn translate_to_sql(
        self,
        options: &Pg2SqliteOptions,
    ) -> Result<Vec<String>, crate::errors::Error> {
        Ok(self.translate(options)?.into_iter().map(|s| s.to_string()).collect())
    }

    /// Builds the schema from the loaded PostgreSQL statements.
    ///
    /// This method constructs a [`ParserDB`] schema from the PostgreSQL
    /// statements that have been added to this translator. The schema can
    /// be reused for multiple reverse translation operations.
    ///
    /// # Returns
    ///
    /// A Result containing the constructed [`ParserDB`] schema.
    ///
    /// # Errors
    ///
    /// * If the schema could not be constructed from the statements.
    ///
    /// # Example
    ///
    /// ```
    /// # use pg2sqlite::pg2sqlite::Pg2Sqlite;
    /// let translator =
    ///     Pg2Sqlite::default().sql("CREATE TABLE users (id UUID PRIMARY KEY, name TEXT);").unwrap();
    /// let schema = translator.build_schema().unwrap();
    /// ```
    pub fn build_schema(&self) -> Result<ParserDB, crate::errors::Error> {
        let normalized_statements = Self::normalize_statements(&self.pg_statements);
        ParserDB::from_statements(normalized_statements, "translation_db".to_owned())
            .map_err(crate::errors::Error::from)
    }

    /// Reverse translates a single SQLite statement to PostgreSQL.
    ///
    /// This method converts a SQLite DML statement (INSERT, UPDATE, DELETE,
    /// SELECT) back to its PostgreSQL equivalent, using the schema built
    /// from the loaded PostgreSQL statements.
    ///
    /// # Arguments
    ///
    /// * `sqlite_stmt` - The SQLite statement to reverse translate.
    /// * `schema` - The schema to use for type recovery and validation.
    /// * `options` - The translation options.
    ///
    /// # Returns
    ///
    /// A Result containing the PostgreSQL statement.
    ///
    /// # Errors
    ///
    /// * [`crate::errors::Error::UnsupportedReverseStatement`] - If the
    ///   statement is not a DML statement.
    /// * [`crate::errors::Error::RlsTableDetected`] - If the statement
    ///   references an RLS backing table.
    ///
    /// # Example
    ///
    /// ```
    /// # use pg2sqlite::pg2sqlite::Pg2Sqlite;
    /// # use pg2sqlite::options::Pg2SqliteOptions;
    /// # use sqlparser::{dialect::SQLiteDialect, parser::Parser};
    /// let translator =
    ///     Pg2Sqlite::default().sql("CREATE TABLE users (id UUID PRIMARY KEY, name TEXT);").unwrap();
    /// let schema = translator.build_schema().unwrap();
    /// let options = Pg2SqliteOptions::default();
    ///
    /// let sqlite_stmt =
    ///     Parser::parse_sql(&SQLiteDialect {}, "SELECT * FROM users").unwrap().pop().unwrap();
    /// let pg_stmt = translator.reverse_translate(&sqlite_stmt, &schema, &options).unwrap();
    /// ```
    pub fn reverse_translate(
        &self,
        sqlite_stmt: &Statement,
        schema: &ParserDB,
        options: &Pg2SqliteOptions,
    ) -> Result<Statement, crate::errors::Error> {
        sqlite_stmt.reverse_translate(schema, options)
    }

    /// Parses SQLite SQL and reverse translates it to PostgreSQL statements.
    ///
    /// This is a convenience method that parses a SQLite SQL string and
    /// reverse translates all statements to PostgreSQL.
    ///
    /// # Arguments
    ///
    /// * `sqlite_sql` - The SQLite SQL string to parse and reverse translate.
    /// * `schema` - The schema to use for type recovery and validation.
    /// * `options` - The translation options.
    ///
    /// # Returns
    ///
    /// A Result containing a vector of PostgreSQL statements.
    ///
    /// # Errors
    ///
    /// * [`crate::errors::Error::ParserError`] - If the SQL could not be
    ///   parsed.
    /// * [`crate::errors::Error::UnsupportedReverseStatement`] - If any
    ///   statement is not a DML statement.
    /// * [`crate::errors::Error::RlsTableDetected`] - If any statement
    ///   references an RLS backing table.
    ///
    /// # Example
    ///
    /// ```
    /// # use pg2sqlite::pg2sqlite::Pg2Sqlite;
    /// # use pg2sqlite::options::Pg2SqliteOptions;
    /// let translator =
    ///     Pg2Sqlite::default().sql("CREATE TABLE users (id UUID PRIMARY KEY, name TEXT);").unwrap();
    /// let schema = translator.build_schema().unwrap();
    /// let options = Pg2SqliteOptions::default();
    ///
    /// let pg_stmts = translator
    ///     .reverse_sql(
    ///         "SELECT * FROM users; INSERT INTO users VALUES ('abc', 'test');",
    ///         &schema,
    ///         &options,
    ///     )
    ///     .unwrap();
    /// assert_eq!(pg_stmts.len(), 2);
    /// ```
    pub fn reverse_sql(
        &self,
        sqlite_sql: &str,
        schema: &ParserDB,
        options: &Pg2SqliteOptions,
    ) -> Result<Vec<Statement>, crate::errors::Error> {
        let stmts =
            sqlparser::parser::Parser::parse_sql(&sqlparser::dialect::SQLiteDialect {}, sqlite_sql)
                .map_err(|e| crate::errors::Error::ParserError(sqlite_sql.to_owned(), e))?;

        stmts.iter().map(|stmt| self.reverse_translate(stmt, schema, options)).collect()
    }
}
