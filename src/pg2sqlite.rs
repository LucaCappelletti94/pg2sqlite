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

#[cfg(feature = "git")]
use git2::Repository;
use sql_traits::structs::{AccessResolution, ParseOptions, ParserDB, ParserDBIngestor};
use sqlparser::{
    ast::{
        AlterTableOperation, CreateIndex, Expr, Ident, IndexType, ObjectName, ObjectNamePart,
        ObjectType, RenameTableNameKind, Statement, Value, ValueWithSpan, visit_expressions,
    },
    dialect::GenericDialect,
};
#[cfg(feature = "git")]
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
    options::{Pg2SqliteOptions, TranslationContext},
    prelude::ReverseTranslator,
    traits::translator::TranslatorWithContext,
};

/// Registers a GIN / GiST FTS index so the `@@ to_tsquery` rewrite can gate on
/// a declared index. Without the catalog the rewrite referenced an undeclared
/// `<table>_fts` virtual table, causing a runtime error.
fn register_fts_index(create_index: &CreateIndex, context: &mut TranslationContext<'_>) {
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
        context.add_fts_index(&table_name, &col);
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
    context: &mut TranslationContext<'_>,
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
        context.add_spatial_index(&table_name, &col);
    }
}

fn populate_prewalk_catalogs(statements: &[Statement], context: &mut TranslationContext<'_>) {
    for statement in statements {
        register_declared_object_name(statement, context);
        if let Statement::CreateIndex(create_index) = statement {
            register_fts_index(create_index, context);
        }
    }
}

fn populate_spatial_index_catalog(
    statements: &[Statement],
    schema: &ParserDB,
    context: &mut TranslationContext<'_>,
) {
    if !context.is_sqlitegis_enabled() {
        return;
    }
    for statement in statements {
        if let Statement::CreateIndex(create_index) = statement {
            register_spatial_index(create_index, schema, context);
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

/// Registers names from the raw statement because translations can need a
/// declaration before sequential schema ingestion reaches it.
fn register_declared_object_name(statement: &Statement, context: &mut TranslationContext<'_>) {
    let name = match statement {
        Statement::CreateTable(create_table) => Some(&create_table.name),
        Statement::CreateView(create_view) => Some(&create_view.name),
        Statement::CreateTrigger(create_trigger) => Some(&create_trigger.name),
        Statement::CreateIndex(create_index) => create_index.name.as_ref(),
        _ => None,
    };
    if let Some(name) = name {
        context.add_declared_object_name(last_ident_value_or_display(name));
    }

    if let Statement::CreateTrigger(create_trigger) = statement
        && let Some(exec_body) = &create_trigger.exec_body
    {
        context.add_trigger_function_name(last_ident_value_or_display(&exec_body.func_desc.name));
    }
}

/// Struct to translate between a `PostgreSQL` entry and a `SQLite` entry.
#[derive(Debug, Clone, Default)]
pub struct Pg2Sqlite {
    /// The set of `PostgreSQL` statements to be translated.
    pg_statements: Vec<Statement>,
}

struct PreparedEpoch {
    start: usize,
    end: usize,
    schema: ParserDB,
}

struct PreparedSchema {
    schema: Result<ParserDB, crate::errors::Error>,
    options: Pg2SqliteOptions,
    epochs: Vec<PreparedEpoch>,
    had_rls_tables: bool,
    complete: bool,
}

struct PreparedStatements {
    schema: ParserDB,
    options: Pg2SqliteOptions,
    statements: Vec<Vec<Statement>>,
    audit_table: Option<Statement>,
    warnings: Vec<crate::warnings::TranslationWarning>,
}

struct PreparedTranslation {
    statements: Vec<Statement>,
    warnings: Vec<crate::warnings::TranslationWarning>,
}

impl Pg2Sqlite {
    /// Refuses a table that is both renamed and placed under row level
    /// security.
    ///
    /// Row level security is realised as a backing table, two views, and five
    /// triggers all named after the table. A SQLite table rename cannot move
    /// that generated set together.
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
            return Err(crate::errors::Error::forward_refusal(format!(
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

    /// Starts incremental schema ingestion with external grants allowed.
    fn translation_ingestor() -> ParserDBIngestor {
        ParseOptions::default()
            .with_access_resolution(AccessResolution::OpenWorld)
            .ingestor::<GenericDialect>("translation_db".to_owned())
    }

    fn build_translation_schema(
        statements: Vec<Statement>,
    ) -> Result<ParserDB, crate::errors::Error> {
        let mut input = Self::translation_ingestor();
        for statement in statements {
            input = input.apply_statement(statement)?;
        }
        Ok(input.finish())
    }

    fn schema_statement_is_ignored(statement: &Statement) -> bool {
        match statement {
            Statement::CreateIndex(_) | Statement::CreateTrigger(_) | Statement::DropTrigger(_) => {
                true
            }
            Statement::Drop { object_type, .. } => {
                !matches!(object_type, ObjectType::Table | ObjectType::View)
            }
            _ => false,
        }
    }

    fn schema_ends_epoch(statement: &Statement) -> bool {
        if let Statement::AlterTable(alter_table) = statement {
            return alter_table
                .operations
                .iter()
                .any(|operation| matches!(operation, AlterTableOperation::RenameTable { .. }));
        }
        matches!(
            statement,
            Statement::Drop { object_type: ObjectType::Table | ObjectType::View, .. }
                | Statement::RenameTable(_)
        )
    }

    fn manifest_statement_needs_translation(statement: &Statement) -> bool {
        !matches!(
            statement,
            Statement::Query(_)
                | Statement::Insert(_)
                | Statement::Update(_)
                | Statement::Delete(_)
                | Statement::Merge(_)
                | Statement::Truncate(_)
                | Statement::Copy { .. }
                | Statement::Directory { .. }
                | Statement::ExportData(_)
                | Statement::Unload { .. }
                | Statement::LoadData { .. }
                | Statement::CopyIntoSnowflake { .. }
                | Statement::Put { .. }
                | Statement::Remove { .. }
                | Statement::List { .. }
        )
    }

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
    #[must_use]
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
                .map_err(|source| {
                    crate::errors::Error::from(crate::errors::SqlParseError::new(
                        sql.to_owned(),
                        source,
                    ))
                })?;
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
    pub fn ups_until<D: AsRef<std::path::Path>, S: AsRef<std::path::Path>>(
        directory: D,
        stop_at: S,
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
    /// Only available with the `git` feature.
    ///
    /// # Errors
    ///
    /// Returns an error if the repository cannot be cloned or migrations cannot
    /// be read.
    #[cfg(feature = "git")]
    pub fn from_git(url: &str) -> Result<Self, crate::errors::Error> {
        let temp_dir = TempDir::new()?;
        Repository::clone(url, temp_dir.path())
            .map_err(|e| crate::errors::Error::GitError(e.to_string()))?;
        Self::ups(temp_dir.path())
    }

    fn prepare_schema(
        &self,
        options: &Pg2SqliteOptions,
        initial_schema: Option<&ParserDB>,
    ) -> Result<PreparedSchema, crate::errors::Error> {
        use sql_traits::traits::DatabaseLike;

        let statements = &self.pg_statements;
        crate::impls::translator_impls::rls::ensure_usable_rls_table_suffix(options)?;
        Self::reject_rename_of_secured_table(statements)?;

        let options = options.clone();
        let had_rls_tables = match initial_schema {
            Some(schema) => schema.has_rls_tables()?,
            None => false,
        };
        let complete = initial_schema.is_some();
        let mut input = initial_schema
            .map_or_else(Self::translation_ingestor, |schema| schema.clone().into_ingestor());
        let mut epochs = Vec::new();
        let mut start = 0;
        let schema = loop {
            if start >= statements.len() {
                break Ok(input.finish());
            }
            let boundary = statements[start..]
                .iter()
                .position(Self::schema_ends_epoch)
                .map(|offset| start + offset);
            let end = boundary.map_or(statements.len(), |index| index + 1);
            let schema_end = boundary.unwrap_or(end);
            for statement in &statements[start..schema_end] {
                if !Self::schema_statement_is_ignored(statement) {
                    input = input.apply_statement(statement.clone())?;
                }
            }
            epochs.push(PreparedEpoch { start, end, schema: input.snapshot() });
            if let Some(index) = boundary {
                match input.apply_statement(statements[index].clone()) {
                    Ok(next) => input = next,
                    Err(error) => break Err(error.into()),
                }
            }
            start = end;
        };

        Ok(PreparedSchema { schema, options, epochs, had_rls_tables, complete })
    }

    fn translate_prepared(
        &self,
        prepared: PreparedSchema,
        mut should_translate: impl FnMut(&Statement) -> bool,
    ) -> Result<PreparedStatements, crate::errors::Error> {
        use sql_traits::traits::DatabaseLike;

        let PreparedSchema { schema, options, epochs, had_rls_tables, complete } = prepared;
        let statements = &self.pg_statements;
        let mut context = if complete {
            TranslationContext::with_complete_schema(&options)
        } else {
            TranslationContext::new(&options)
        };
        populate_prewalk_catalogs(statements, &mut context);

        let mut warnings = Vec::new();
        let mut translated = Vec::with_capacity(statements.len());
        for epoch in &epochs {
            populate_spatial_index_catalog(
                &statements[epoch.start..epoch.end],
                &epoch.schema,
                &mut context,
            );
            for statement in &statements[epoch.start..epoch.end] {
                if should_translate(statement) {
                    let mut emit = |warning| warnings.push(warning);
                    translated.push(statement.translate_with_warnings(
                        &epoch.schema,
                        &context,
                        &mut emit,
                    )?);
                } else {
                    translated.push(Vec::new());
                }
            }
        }

        let schema = schema?;
        let audit_table = (!had_rls_tables && schema.has_rls_tables()?)
            .then(|| options.get_rls_audit_table_name())
            .flatten()
            .map(generate_rls_audit_table);

        reject_name_collisions(
            audit_table
                .iter()
                .map(|statement| (Source::Generated("the row-security audit table"), statement))
                .chain(sourced(statements, &translated)),
        )?;

        Ok(PreparedStatements { schema, options, statements: translated, audit_table, warnings })
    }

    fn prepare_translation(
        &self,
        options: &Pg2SqliteOptions,
        initial_schema: Option<&ParserDB>,
    ) -> Result<PreparedTranslation, crate::errors::Error> {
        let prepared = self.prepare_schema(options, initial_schema)?;
        let PreparedStatements { schema: _, options: _, statements, audit_table, warnings } =
            self.translate_prepared(prepared, |_| true)?;

        let mut result: Vec<Statement> = statements.into_iter().flatten().collect();
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

        Ok(PreparedTranslation { statements: result, warnings })
    }

    /// Translates loaded PostgreSQL statements to SQLite.
    ///
    /// Warnings about dropped or downgraded constructs are discarded on this
    /// path. Use [`translate_with_report`](Self::translate_with_report) to
    /// collect them alongside the statements.
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
        &self,
        options: &Pg2SqliteOptions,
    ) -> Result<Vec<Statement>, crate::errors::Error> {
        Ok(self.prepare_translation(options, None)?.statements)
    }

    /// Translates loaded PostgreSQL statements using `schema` as the complete
    /// starting database state.
    ///
    /// Loaded schema statements update that state before later statements are
    /// translated.
    ///
    /// # Errors
    ///
    /// Returns an error if schema ingestion or translation fails.
    pub fn translate_with_schema(
        &self,
        schema: &ParserDB,
        options: &Pg2SqliteOptions,
    ) -> Result<Vec<Statement>, crate::errors::Error> {
        Ok(self.prepare_translation(options, Some(schema))?.statements)
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
    /// # Example
    ///
    /// ```
    /// # use pg2sqlite::prelude::*;
    /// let report = Pg2Sqlite::default()
    ///     .sql("CREATE TABLE t (a INT);")?
    ///     .translate_with_report(&Pg2SqliteOptions::default())?;
    /// assert_eq!(report.statements.len(), 1);
    /// assert!(report.warnings.is_empty());
    /// # Ok::<(), Error>(())
    /// ```
    ///
    /// [`TranslationReport`]: crate::warnings::TranslationReport
    pub fn translate_with_report(
        &self,
        options: &Pg2SqliteOptions,
    ) -> Result<crate::warnings::TranslationReport, crate::errors::Error> {
        let PreparedTranslation { statements, warnings, .. } =
            self.prepare_translation(options, None)?;
        Ok(crate::warnings::TranslationReport { statements, warnings })
    }

    /// Translates against a complete starting `schema` and returns statements
    /// with their warnings.
    ///
    /// # Errors
    ///
    /// Returns an error if schema ingestion or translation fails.
    pub fn translate_with_report_and_schema(
        &self,
        schema: &ParserDB,
        options: &Pg2SqliteOptions,
    ) -> Result<crate::warnings::TranslationReport, crate::errors::Error> {
        let PreparedTranslation { statements, warnings, .. } =
            self.prepare_translation(options, Some(schema))?;
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
    ///
    /// # Example
    ///
    /// ```
    /// # use pg2sqlite::prelude::*;
    /// let sql = Pg2Sqlite::default()
    ///     .sql("CREATE TABLE t (a INT);")?
    ///     .translate_to_sql(&Pg2SqliteOptions::default())?;
    /// assert_eq!(sql, ["CREATE TABLE t (a INTEGER) STRICT"]);
    /// # Ok::<(), Error>(())
    /// ```
    pub fn translate_to_sql(
        &self,
        options: &Pg2SqliteOptions,
    ) -> Result<Vec<String>, crate::errors::Error> {
        Ok(self.translate(options)?.into_iter().map(|s| s.to_string()).collect())
    }

    /// Translates against a complete starting `schema` and returns SQL strings.
    ///
    /// # Errors
    ///
    /// Returns an error if schema ingestion or translation fails.
    pub fn translate_to_sql_with_schema(
        &self,
        schema: &ParserDB,
        options: &Pg2SqliteOptions,
    ) -> Result<Vec<String>, crate::errors::Error> {
        Ok(self
            .translate_with_schema(schema, options)?
            .into_iter()
            .map(|statement| statement.to_string())
            .collect())
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
        Self::build_translation_schema(self.pg_statements.clone())
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
    /// # use pg2sqlite::{pg2sqlite::Pg2Sqlite, options::Pg2SqliteOptions, manifest::WrapperKind, traits::UuidRepresentation};
    /// let options =
    ///     Pg2SqliteOptions::default().with_uuid_representation(UuidRepresentation::Text);
    /// let manifest = Pg2Sqlite::default()
    ///     .sql("CREATE TABLE users (id UUID PRIMARY KEY, name TEXT);")
    ///     .unwrap()
    ///     .translation_manifest(&options)
    ///     .unwrap();
    /// assert_eq!(manifest[0].logical, "users");
    /// assert_eq!(manifest[0].wrapper, WrapperKind::Plain);
    /// ```
    ///
    /// A `NUMERIC(p, s)` column stores minor units, so reading it back gives
    /// 1999 where PostgreSQL gave 19.99. The manifest publishes the scale and
    /// the consumer applies it when presenting the value:
    ///
    /// ```
    /// # use pg2sqlite::{pg2sqlite::Pg2Sqlite, options::Pg2SqliteOptions};
    /// let manifest = Pg2Sqlite::default()
    ///     .sql("CREATE TABLE prices (id INT PRIMARY KEY, amount NUMERIC(10, 2));")
    ///     .unwrap()
    ///     .translation_manifest(&Pg2SqliteOptions::default())
    ///     .unwrap();
    /// let amount = manifest[0].columns.iter().find(|c| c.name == "amount").unwrap();
    /// assert_eq!(amount.minor_unit_scale, Some(2));
    ///
    /// let stored = 1999_i64;
    /// let scale = 10_i64.pow(amount.minor_unit_scale.unwrap());
    /// assert_eq!(format!("{}.{:02}", stored / scale, stored % scale), "19.99");
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

        let prepared = self.prepare_schema(options, None)?;
        let PreparedStatements { schema, options, .. } =
            self.translate_prepared(prepared, Self::manifest_statement_needs_translation)?;
        let schema = &schema;
        let options = &options;

        let role = options.get_session_user_role().and_then(|name| schema.role(name));

        let mut entries = Vec::new();
        for table in schema.tables() {
            if let Some(role) = role
                && !table.can_select(role, schema)?
            {
                continue;
            }

            let logical = table.table_name().to_string();
            let physical = resolve_trigger_table_name(&logical, table, schema, options)?;
            let readonly = match role {
                Some(role) => !table.can_write(role, schema)?,
                None => false,
            };
            let wrapper = if table.has_row_level_security(schema)? {
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
                .columns(schema)?
                .map(|column| {
                    let minor_unit_scale =
                        crate::impls::translator_impls::data_type::exact_numeric_info(
                            &column.attribute().data_type,
                        )
                        .map(crate::impls::translator_impls::data_type::numeric_precision_and_scale)
                        .transpose()?
                        .map(|(_, scale)| scale);
                    Ok(ColumnManifestEntry {
                        name: column.column_name().to_string(),
                        minor_unit_scale,
                    })
                })
                .collect::<Result<Vec<_>, crate::errors::Error>>()?;

            entries.push(TableManifestEntry { logical, physical, wrapper, columns });
        }

        Ok(entries)
    }

    /// Reverse translates a single SQLite DML statement to PostgreSQL.
    ///
    /// Carries the same case-sensitive-matching requirement as
    /// [`Pg2Sqlite::reverse_sql`], which states it in full: the statement must
    /// have run on a connection carrying `PRAGMA case_sensitive_like = true`,
    /// or a plain `LIKE` handed back here matches fewer rows in PostgreSQL
    /// than it did in SQLite.
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
        let context = TranslationContext::new(options);
        sqlite_stmt.reverse_translate(schema, &context)
    }

    /// Parses SQLite SQL and reverse translates all statements to PostgreSQL.
    ///
    /// Identifiers are re-quoted to PostgreSQL double quotes (SQLite also
    /// accepts backtick and bracket quoting). Only the quote style changes: the
    /// identifier text is preserved verbatim, so a mixed-case SQLite identifier
    /// becomes a case-sensitive PostgreSQL identifier with the same spelling,
    /// which lines up only under a shared schema.
    ///
    /// # The caller's side of the bargain: case-sensitive matching
    ///
    /// The SQL given here must have run on a connection carrying `PRAGMA
    /// case_sensitive_like = true`. SQLite's `LIKE` ignores letter case unless
    /// a connection says otherwise, PostgreSQL's never does, and a plain `LIKE`
    /// is handed back as a plain `LIKE`, so without that setting the
    /// PostgreSQL statement matches fewer rows than the SQLite one did.
    ///
    /// Forward translation writes that pragma into its own script, but only
    /// into a script that itself contains a `LIKE`, and a pragma is connection
    /// state rather than anything held in the database file. An application
    /// that opens the replica later therefore has to set it, and nothing here
    /// can tell whether it did.
    ///
    /// Handing back `ILIKE` instead would not be the safer default. SQLite
    /// folds the ASCII letters only, so `'Ä' LIKE 'ä'` is false where
    /// PostgreSQL's `ILIKE` is true, which trades one wrong answer for another
    /// and adds a divergence outside ASCII.
    ///
    /// ```
    /// # use pg2sqlite::pg2sqlite::Pg2Sqlite;
    /// # use pg2sqlite::options::Pg2SqliteOptions;
    /// # let translator =
    /// #     Pg2Sqlite::default().sql("CREATE TABLE t (id INT PRIMARY KEY, s TEXT);").unwrap();
    /// # let schema = translator.build_schema().unwrap();
    /// let pg = translator
    ///     .reverse_sql("SELECT s FROM t WHERE s LIKE 'a%'", &schema, &Pg2SqliteOptions::default())
    ///     .unwrap();
    /// assert_eq!(pg[0].to_string(), "SELECT s FROM t WHERE s LIKE 'a%'");
    /// ```
    ///
    /// # A zone-free timestamp gains a zone
    ///
    /// SQLite's one-argument `datetime(x)` converts to UTC when the value
    /// carries an offset and otherwise only tidies the printed form. It comes
    /// back as `x AT TIME ZONE 'UTC'`, which is exact for the first case and,
    /// for the second, right about the clock reading while turning a plain
    /// timestamp into a zone-aware one: SQLite answers `2026-08-08 15:04:05`
    /// where PostgreSQL answers `2026-08-08 15:04:05+00`.
    ///
    /// Splitting on the column's declared type would fix that and break the
    /// other side, since forward translation emits this same call for `AT TIME
    /// ZONE 'UTC'` over either kind of operand, so no single reversal is right
    /// about both.
    ///
    /// # Errors
    ///
    /// Returns [`crate::errors::Error::SqlParse`] if parsing fails,
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
                .map_err(|source| {
                    crate::errors::Error::from(crate::errors::SqlParseError::new(
                        sqlite_sql.to_owned(),
                        source,
                    ))
                })?;

        stmts.iter().map(|stmt| self.reverse_translate(stmt, schema, options)).collect()
    }
}
