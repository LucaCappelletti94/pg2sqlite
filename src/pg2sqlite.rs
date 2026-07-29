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
use core::ops::ControlFlow;
#[cfg(feature = "std")]
use std::path::PathBuf;

#[cfg(feature = "std")]
use git2::Repository;
use sql_traits::structs::ParserDB;
use sqlparser::ast::{
    AlterTableOperation, Expr, Ident, IndexType, ObjectName, ObjectNamePart, Statement, Value,
    ValueWithSpan, visit_expressions,
};
#[cfg(feature = "std")]
use tempfile::TempDir;

use crate::{
    impls::{
        object_name::last_ident_value_or_display,
        translator_impls::{
            create_index::{FtsTranslation, analyze_fts_index},
            postgis,
            rls::generate_rls_audit_table,
        },
    },
    options::Pg2SqliteOptions,
    prelude::{ReverseTranslator, Translator},
    traits::TranslationOptions,
};

/// Pre-walks for GIN / GiST FTS indexes and populates the FTS-index catalog so
/// the `@@ to_tsquery` rewrite can gate on a declared index. Without the
/// catalog the rewrite referenced an undeclared `<table>_fts` virtual table,
/// causing a runtime error.
fn populate_fts_index_catalog(statements: &[Statement], options: &mut Pg2SqliteOptions) {
    for stmt in statements {
        let Statement::CreateIndex(create_index) = stmt else {
            continue;
        };
        if !matches!(
            create_index.using,
            Some(sqlparser::ast::IndexType::GIN | sqlparser::ast::IndexType::GiST)
        ) {
            continue;
        }
        let FtsTranslation::Fts5 { columns, .. } = analyze_fts_index(create_index) else {
            continue;
        };
        let table_name = last_ident_value_or_display(&create_index.table_name);
        for col in columns {
            options.add_fts_index(&table_name, &col);
        }
    }
}

/// Pre-walks GiST indexes over `geometry`/`geography` columns to populate the
/// spatial-index catalog that drives query-time predicate rewriting.
///
/// A classification error is dropped on purpose: the per-statement translation
/// re-runs the same classifier and reports it with full context.
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

/// `PRAGMA case_sensitive_like = ON`, which makes SQLite's `LIKE` match
/// PostgreSQL's case-sensitive behaviour.
fn case_sensitive_like_pragma() -> Statement {
    Statement::Pragma {
        name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new("case_sensitive_like"))]),
        value: Some(ValueWithSpan {
            value: Value::Boolean(true),
            span: sqlparser::tokenizer::Span::empty(),
        }),
        is_eq: true,
    }
}

/// True when `statement` contains a `LIKE`, whose matching in SQLite depends on
/// the connection's case sensitivity.
fn statement_contains_like(statement: &Statement) -> bool {
    visit_expressions(statement, |expr| {
        if matches!(expr, Expr::Like { .. }) {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    })
    .is_break()
}

/// Registers every declared object name in `statements` so the read-only
/// deny-trigger pass can reject names that collide with existing objects. Uses
/// raw statements because the translation schema omits index and trigger
/// definitions.
fn populate_declared_object_names(statements: &[Statement], options: &mut Pg2SqliteOptions) {
    for stmt in statements {
        let name = match stmt {
            Statement::CreateTable(create_table) => Some(&create_table.name),
            Statement::CreateView(create_view) => Some(&create_view.name),
            Statement::CreateTrigger(create_trigger) => Some(&create_trigger.name),
            Statement::CreateIndex(create_index) => create_index.name.as_ref(),
            _ => None,
        };
        if let Some(name) = name {
            options.add_declared_object_name(last_ident_value_or_display(name));
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
                // AlterTable with RenameTable triggers a bug in sql-traits: after the
                // rename the internal column arcs still reference the old CreateTable,
                // so a subsequent rls_enabled() call panics on table-not-found. Keep
                // AlterTable statements that only carry non-rename operations (e.g.
                // ENABLE ROW LEVEL SECURITY) so they can update the schema correctly.
                // The rename itself is translated at the statement level without
                // schema mutation.
                if let Statement::AlterTable(alter_table) = statement {
                    return !alter_table
                        .operations
                        .iter()
                        .any(|op| matches!(op, AlterTableOperation::RenameTable { .. }));
                }
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

    /// Parses `sql` as PostgreSQL and appends the resulting statements.
    ///
    /// # Errors
    ///
    /// Returns an error if parsing fails.
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

    /// Reads and parses a PostgreSQL SQL file, appending the statements.
    ///
    /// Only available with the `std` feature.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or parsed.
    #[cfg(feature = "std")]
    pub fn file<P: AsRef<std::path::Path>>(self, path: P) -> Result<Self, crate::errors::Error> {
        let path = path.as_ref();
        let content = std::fs::read_to_string(path)?;
        self.sql(&content)
    }

    /// Loads all `up.sql` migrations found recursively under `directory`.
    ///
    /// Only available with the `std` feature.
    ///
    /// # Errors
    ///
    /// Returns an error if any migration cannot be read or parsed.
    #[cfg(feature = "std")]
    pub fn ups<P: AsRef<std::path::Path>>(directory: P) -> Result<Self, crate::errors::Error> {
        let up_sql_paths = Self::sorted_up_sql_paths(directory.as_ref())?;
        Self::from_migration_paths(up_sql_paths)
    }

    /// Loads `up.sql` migrations under `directory`, stopping at and including
    /// `stop_at`.
    ///
    /// Only available with the `std` feature.
    ///
    /// # Errors
    ///
    /// Returns an error if any migration cannot be read, parsed, or if
    /// `stop_at` is not found.
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

    /// Clones the git repository at `url` and loads its migrations.
    ///
    /// Only available with the `std` feature.
    ///
    /// # Errors
    ///
    /// Returns an error if the repository cannot be cloned or migrations cannot
    /// be read.
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

        if !options.is_dangling_foreign_keys_allowed() {
            schema.validate_foreign_key_targets()?;
        }

        // Pre-walk for spatial-index DDL so that the same translation unit's
        // SELECTs can rewrite `ST_*` predicates over indexed columns through
        // the rtree shadow. Only fires when SQLiteGIS translation is enabled;
        // skips errors (an unsupported GiST will surface its own error when
        // the statement itself is translated below).
        let mut options = options.clone();
        if options.is_sqlitegis_enabled() {
            populate_spatial_index_catalog(&normalized_statements, &schema, &mut options);
        }
        // Always-on: FTS5 rewrite gating doesn't depend on a runtime
        // extension toggle, only on the schema's declared GIN/GiST
        // indexes. Populates `options.fts_indexes`.
        populate_fts_index_catalog(&normalized_statements, &mut options);
        populate_declared_object_names(&normalized_statements, &mut options);
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

        // PostgreSQL's LIKE is case-sensitive. SQLite's is case-insensitive for
        // ASCII unless the connection says otherwise, and no expression-level
        // rewrite fixes that: a BLOB operand stops matching wildcards entirely
        // and GLOB cannot express a pattern computed at runtime or an ESCAPE
        // clause. So the script configures the connection it is applied to.
        // ILIKE is unaffected because it lowercases both operands.
        //
        // The pragma is connection state, not database state, so a LIKE that is
        // evaluated later than the script (inside a CHECK constraint, a trigger
        // body, or a view) still depends on the pragma being set on whichever
        // connection runs the write.
        if result.iter().any(statement_contains_like) {
            result.insert(0, case_sensitive_like_pragma());
        }

        Ok(result)
    }

    /// Translates loaded PostgreSQL statements to SQLite.
    ///
    /// # Errors
    ///
    /// Returns an error if translation fails.
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

    /// Translates the loaded PostgreSQL statements to SQLite and returns
    /// a [`TranslationReport`] containing both the statements and any
    /// warnings collected during translation. Warnings flag constructs
    /// that have no SQLite equivalent and were dropped or downgraded.
    ///
    /// # Errors
    ///
    /// * If parsing or translation fails.
    ///
    /// [`TranslationReport`]: crate::warnings::TranslationReport
    pub fn translate_with_report(
        self,
        options: &Pg2SqliteOptions,
    ) -> Result<crate::warnings::TranslationReport, crate::errors::Error> {
        let scope = crate::warnings::CollectorScope::install();
        let statements = self.translate_internal(options)?;
        let warnings = scope.take();
        Ok(crate::warnings::TranslationReport { statements, warnings })
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

    /// Builds a [`ParserDB`] schema from the loaded PostgreSQL statements,
    /// reusable for multiple reverse translation operations.
    ///
    /// # Errors
    ///
    /// Returns an error if schema construction fails.
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

    /// Logical-to-physical table map produced under `options`,
    /// one [`TableManifestEntry`](crate::manifest::TableManifestEntry) per
    /// emitted table. A role-configured `options` omits tables the role
    /// cannot SELECT.
    ///
    /// # Errors
    ///
    /// Returns an error if schema construction fails.
    ///
    /// # Example
    ///
    /// ```
    /// # use pg2sqlite::{pg2sqlite::Pg2Sqlite, options::Pg2SqliteOptions, manifest::WrapperKind};
    /// let manifest = Pg2Sqlite::default()
    ///     .sql("CREATE TABLE users (id UUID PRIMARY KEY, name TEXT);")
    ///     .unwrap()
    ///     .translation_manifest(&Pg2SqliteOptions::default())
    ///     .unwrap();
    /// assert_eq!(manifest[0].logical, "users");
    /// assert_eq!(manifest[0].wrapper, WrapperKind::Plain);
    /// ```
    pub fn translation_manifest(
        &self,
        options: &Pg2SqliteOptions,
    ) -> Result<Vec<crate::manifest::TableManifestEntry>, crate::errors::Error> {
        use sql_traits::traits::{DatabaseLike, TableLike};

        use crate::{
            impls::translator_impls::rls::resolve_trigger_table_name,
            manifest::{TableManifestEntry, WrapperKind},
        };

        let schema = self.build_schema()?;

        if !options.is_dangling_foreign_keys_allowed() {
            schema.validate_foreign_key_targets()?;
        }

        let role = options.get_session_user_role().and_then(|name| schema.role(name));

        let mut entries = Vec::new();
        for table in schema.tables() {
            if role.is_some_and(|role| !table.can_select(role, &schema)) {
                continue;
            }

            let logical = table.table_name().to_string();
            let physical = resolve_trigger_table_name(&logical, table, &schema, options);
            let wrapper = if table.has_row_level_security(&schema) {
                WrapperKind::RlsView
            } else if role.is_some_and(|role| !table.can_write(role, &schema)) {
                WrapperKind::ReadOnly
            } else {
                WrapperKind::Plain
            };

            entries.push(TableManifestEntry { logical, physical, wrapper });
        }

        Ok(entries)
    }

    /// Reverse translates a single SQLite DML statement to PostgreSQL.
    ///
    /// # Errors
    ///
    /// Returns [`crate::errors::Error::UnsupportedReverseStatement`] for
    /// non-DML statements or [`crate::errors::Error::RlsTableDetected`] for
    /// RLS backing table references.
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

    /// Parses SQLite SQL and reverse translates all statements to PostgreSQL.
    ///
    /// Identifiers are re-quoted to PostgreSQL double quotes (SQLite also
    /// accepts backtick and bracket quoting). Only the quote style changes: the
    /// identifier text is preserved verbatim, so a mixed-case SQLite identifier
    /// becomes a case-sensitive PostgreSQL identifier with the same spelling,
    /// which lines up only under a shared schema.
    ///
    /// # Errors
    ///
    /// Returns [`crate::errors::Error::ParserError`] if parsing fails,
    /// [`crate::errors::Error::UnsupportedReverseStatement`] for non-DML
    /// statements, or [`crate::errors::Error::RlsTableDetected`] for RLS
    /// backing table references.
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
