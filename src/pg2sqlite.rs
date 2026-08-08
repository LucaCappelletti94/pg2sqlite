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
use sql_traits::structs::{AccessResolution, ParseOptions, ParserDB};
use sqlparser::ast::{
    AlterTableOperation, CreateIndex, Expr, Ident, IndexType, ObjectName, ObjectNamePart,
    RenameTableNameKind, Statement, Value, ValueWithSpan, visit_expressions,
};
#[cfg(feature = "std")]
use tempfile::TempDir;

use crate::{
    impls::{
        emitted_namespace::{Source, reject_name_collisions, sourced},
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

/// Registers a GIN / GiST FTS index so the `@@ to_tsquery` rewrite can gate on
/// a declared index. Without the catalog the rewrite referenced an undeclared
/// `<table>_fts` virtual table, causing a runtime error.
fn register_fts_index(create_index: &CreateIndex, options: &mut Pg2SqliteOptions) {
    if !matches!(
        create_index.using,
        Some(sqlparser::ast::IndexType::GIN | sqlparser::ast::IndexType::GiST)
    ) {
        return;
    }
    let FtsTranslation::Fts5 { columns, .. } = analyze_fts_index(create_index) else {
        return;
    };
    let table_name = last_ident_value_or_display(&create_index.table_name);
    for col in columns {
        options.add_fts_index(&table_name, &col);
    }
}

/// Registers a GiST index over `geometry`/`geography` columns in the
/// spatial-index catalog that drives query-time predicate rewriting.
///
/// A classification error is dropped on purpose: the per-statement translation
/// re-runs the same classifier and reports it with full context.
fn register_spatial_index(
    create_index: &CreateIndex,
    schema: &ParserDB,
    options: &mut Pg2SqliteOptions,
) {
    if !matches!(create_index.using, Some(IndexType::GiST)) {
        return;
    }
    let Ok(Some(spatial_columns)) = postgis::classify_gist_spatial_columns(create_index, schema)
    else {
        return;
    };
    let table_name = last_ident_value_or_display(&create_index.table_name);
    for col in spatial_columns {
        options.add_spatial_index(&table_name, &col);
    }
}

/// Populates every catalog that a statement's translation reads but that only
/// another statement in the same unit can declare, so a `SELECT` can be
/// rewritten against an index written after it.
///
/// One traversal serves all three: they only read `statements`, and none
/// observes what the others record.
///
/// Spatial registration is gated because the rewrite it feeds targets an
/// extension that may not be loaded. The FTS catalog has no such toggle, since
/// its rewrite depends on the declared indexes alone.
fn populate_prewalk_catalogs(
    statements: &[Statement],
    schema: &ParserDB,
    options: &mut Pg2SqliteOptions,
) {
    let spatial_enabled = options.is_sqlitegis_enabled();
    for statement in statements {
        register_declared_object_name(statement, options);

        let Statement::CreateIndex(create_index) = statement else {
            continue;
        };
        if spatial_enabled {
            register_spatial_index(create_index, schema, options);
        }
        register_fts_index(create_index, options);
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

/// Registers `statement`'s declared object name, so the read-only deny-trigger
/// pass can reject names that collide with existing objects, and the function a
/// trigger executes, so the `CREATE FUNCTION` arm knows which definitions are
/// realised inside a trigger rather than lost. Both read the raw statement
/// because [`Pg2Sqlite::schema_statements_for_translation`] keeps triggers out
/// of the translation schema, and that exclusion is load-bearing, so this
/// catalog cannot be replaced by reading names back off the schema.
fn register_declared_object_name(statement: &Statement, options: &mut Pg2SqliteOptions) {
    let name = match statement {
        Statement::CreateTable(create_table) => Some(&create_table.name),
        Statement::CreateView(create_view) => Some(&create_view.name),
        Statement::CreateTrigger(create_trigger) => Some(&create_trigger.name),
        Statement::CreateIndex(create_index) => create_index.name.as_ref(),
        _ => None,
    };
    if let Some(name) = name {
        options.add_declared_object_name(last_ident_value_or_display(name));
    }

    if let Statement::CreateTrigger(create_trigger) = statement
        && let Some(exec_body) = &create_trigger.exec_body
    {
        options.add_trigger_function_name(last_ident_value_or_display(&exec_body.func_desc.name));
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

    /// Refuses a table that is both renamed and placed under row level
    /// security.
    ///
    /// Row level security is realised as a backing table, two views, and five
    /// triggers all named after the table, so a rename would have to move the
    /// whole set together. Neither order works today. With the rename above,
    /// the security statements name a table the schema does not carry,
    /// because [`Pg2Sqlite::schema_statements_for_translation`] keeps renames
    /// out. With the rename below, the security setting is applied and the
    /// emitted rename then lands on the view, which SQLite answers with
    /// `view <name> may not be altered`.
    ///
    /// Refusing is the whole answer rather than a stopgap: resolving a table by
    /// a name it has since lost is ambiguous once a file swaps two names, so
    /// the schema cannot be asked which table a pre-rename statement meant.
    fn reject_rename_of_secured_table(
        statements: &[Statement],
    ) -> Result<(), crate::errors::Error> {
        let mut renamed: Vec<(String, String)> = Vec::new();
        let mut secured: Vec<String> = Vec::new();

        for statement in statements {
            match statement {
                Statement::AlterTable(alter_table) => {
                    let subject = last_ident_value_or_display(&alter_table.name);
                    for operation in &alter_table.operations {
                        match operation {
                            AlterTableOperation::RenameTable { table_name } => {
                                let (RenameTableNameKind::As(name) | RenameTableNameKind::To(name)) =
                                    table_name;
                                renamed.push((subject.clone(), last_ident_value_or_display(name)));
                            }
                            AlterTableOperation::EnableRowLevelSecurity
                            | AlterTableOperation::ForceRowLevelSecurity => {
                                secured.push(subject.clone());
                            }
                            _ => {}
                        }
                    }
                }
                Statement::CreatePolicy(policy) => {
                    secured.push(last_ident_value_or_display(&policy.table_name));
                }
                _ => {}
            }
        }

        for (old_name, new_name) in renamed {
            let Some(name) =
                secured.iter().find(|secured| **secured == old_name || **secured == new_name)
            else {
                continue;
            };
            return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
                "table `{name}` is both renamed (`{old_name}` to `{new_name}`) and placed under \
                 row level security in the same translation unit. Row level security is realised \
                 as a backing table, two views, and five triggers named after the table, and a \
                 rename cannot move them together. Rename the table in an earlier translation \
                 unit than the one that enables row level security on it, or create it under its \
                 final name."
            )));
        }
        Ok(())
    }

    /// Builds the schema every statement is translated against.
    ///
    /// Access resolution is open because a policy or grant naming a role the
    /// input never creates is the normal shape: roles are cluster objects that
    /// `pg_dump` does not emit, and a platform role such as `authenticated`
    /// exists before any migration runs. Closed resolution, which is upstream's
    /// default, refuses those outright.
    fn build_translation_schema(
        statements: Vec<Statement>,
    ) -> Result<ParserDB, crate::errors::Error> {
        ParseOptions::default()
            .with_access_resolution(AccessResolution::OpenWorld)
            .from_statements(statements, "translation_db".to_owned())
            .map_err(Into::into)
    }

    /// The statements that build the one schema snapshot every statement is
    /// translated against.
    ///
    /// A statement is excluded when it needs no schema entry to translate and
    /// including it would replace its own diagnostic with a schema-build
    /// failure. Indexes and triggers translate from the statement alone, and
    /// the shapes this crate refuses, a non-`public` schema qualifier or a
    /// three-part name, are refused with a message naming the construct that a
    /// schema lookup failure would pre-empt.
    ///
    /// Drops and renames are the same case with one addition: a single snapshot
    /// serves every statement, so applying a drop or a rename to it hides the
    /// object from the statements written before it, which then fail to resolve
    /// their own target. `ALTER TABLE ... RENAME TO` becomes a SQLite rename
    /// from the statement alone, and `RENAME TABLE` is refused outright.
    ///
    /// An `ALTER TABLE` carrying only non-rename operations is kept, because
    /// `ENABLE ROW LEVEL SECURITY` and friends must reach the schema.
    fn schema_statements_for_translation(statements: &[Statement]) -> Vec<Statement> {
        statements
            .iter()
            .filter(|statement| {
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
                        | Statement::RenameTable(_)
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
        use sql_traits::traits::DatabaseLike;

        let normalized_statements = Self::normalize_statements(&self.pg_statements);
        Self::reject_rename_of_secured_table(&normalized_statements)?;

        let schema_statements = Self::schema_statements_for_translation(&normalized_statements);
        let schema = Self::build_translation_schema(schema_statements)?;

        let mut options = options.clone();
        populate_prewalk_catalogs(&normalized_statements, &schema, &mut options);
        let options = options;

        let translated = normalized_statements
            .iter()
            .map(|statement| statement.translate(&schema, &options))
            .collect::<Result<Vec<Vec<Statement>>, crate::errors::Error>>()?;

        // If any table has RLS enabled and audit table name is configured,
        // prepend the audit table creation statement
        let audit_table = schema
            .has_rls_tables()?
            .then(|| options.get_rls_audit_table_name())
            .flatten()
            .map(generate_rls_audit_table)
            .transpose()?;

        // Every name the script defines has to be free when it defines it:
        // PostgreSQL has a namespace per schema and names a trigger within its
        // table, SQLite has one of each for the whole database.
        reject_name_collisions(
            audit_table
                .iter()
                .map(|statement| (Source::Generated("the row-security audit table"), statement))
                .chain(sourced(&normalized_statements, &translated)),
        )?;

        let mut result: Vec<Statement> = translated.into_iter().flatten().collect();
        if let Some(audit_table) = audit_table {
            result.insert(0, audit_table);
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
        //
        // This scans the translated statements while the catalog pre-walk above
        // scans the input. The two differ only for ILIKE, which arrives without
        // a LIKE and leaves as one, and which the pragma cannot affect either
        // way, so scanning here emits a pragma an input scan would skip rather
        // than catching one it would miss.
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
        Self::build_translation_schema(normalized_statements)
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
        use sql_traits::traits::{ColumnLike, DatabaseLike, TableLike};

        use crate::{
            impls::translator_impls::rls::resolve_trigger_table_name,
            manifest::{ColumnManifestEntry, TableManifestEntry, WrapperKind},
        };

        let schema = self.build_schema()?;

        let role = options.get_session_user_role().and_then(|name| schema.role(name));

        let mut entries = Vec::new();
        for table in schema.tables() {
            if let Some(role) = role
                && !table.can_select(role, &schema)?
            {
                continue;
            }

            let logical = table.table_name().to_string();
            let physical = resolve_trigger_table_name(&logical, table, &schema, options)?;
            let readonly = match role {
                Some(role) => !table.can_write(role, &schema)?,
                None => false,
            };
            let wrapper = if table.has_row_level_security(&schema)? {
                WrapperKind::RlsView
            } else if readonly {
                WrapperKind::ReadOnly
            } else {
                WrapperKind::Plain
            };

            // Only NUMERIC carries a representation a consumer cannot read off
            // the emitted type, so every other column reports None rather than
            // being omitted, which keeps the list a faithful column order.
            let columns = table
                .columns(&schema)?
                .map(|column| {
                    let minor_unit_scale =
                        crate::impls::translator_impls::data_type::exact_numeric_info(
                            &column.attribute().data_type,
                        )
                        .and_then(|info| {
                            crate::impls::translator_impls::data_type::numeric_precision_and_scale(
                                info,
                            )
                            .ok()
                            .map(|(_, scale)| scale)
                        });
                    ColumnManifestEntry { name: column.column_name().to_string(), minor_unit_scale }
                })
                .collect();

            entries.push(TableManifestEntry { logical, physical, wrapper, columns });
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
