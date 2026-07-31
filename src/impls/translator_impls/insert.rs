//! Implementation of the [`Translator`] trait for the
//! `Insert` type.

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

use sql_traits::{
    structs::ParserDB,
    traits::{ColumnLike, IndexLike, TableLike, UniqueIndexLike},
};
use sqlparser::ast::{Insert, SetExpr, TableObject};

use super::helpers::Forward;
use crate::{
    impls::{
        object_name::{last_ident, last_ident_value_or_display, table_with_implicit_public_lookup},
        shared_helpers::{
            is_default_keyword, translate_on_conflict_do_update, translate_returning,
        },
        translator_impls::{
            rls,
            uuid::{
                is_blob_uuid_representation, maybe_wrap_text_uuid_literal, uuid_columns_of_table,
            },
            vector::{maybe_wrap_text_vector_literal, vector_columns_of_table},
        },
    },
    prelude::{Pg2SqliteOptions, TranslationOptions, Translator},
};

impl Translator for Insert {
    type Schema = ParserDB;
    type Options = Pg2SqliteOptions;
    type SQLiteEntry = Insert;

    fn translate(
        &self,
        schema: &Self::Schema,
        options: &Self::Options,
    ) -> Result<Self::SQLiteEntry, crate::errors::Error> {
        // Replace DEFAULT with the column's declared default BEFORE translating
        // the source, for two reasons. The substituted expression is PostgreSQL
        // SQL and gets translated by the same path as a written-out value
        // rather than needing its own call. And once no INSERT can carry the
        // keyword any further, `translate_values_rows` can refuse a DEFAULT
        // reaching it from anywhere else, which is the only other place the
        // parser accepts one.
        let mut prepared = self.clone();
        substitute_default_values(&mut prepared, schema, options)?;

        let source = prepared
            .source
            .as_ref()
            .map(|q| q.translate(schema, options))
            .transpose()?
            .map(Box::new);

        let returning = translate_returning::<Forward>(self.returning.as_ref(), schema, options)?;

        let mut insert = Insert { source, returning, ..prepared };

        // INSERT INTO <RLS view> ... RETURNING ...: rewrite to target
        // the backing table so RETURNING surfaces the row that was
        // actually written (the INSTEAD OF view trigger writes to the
        // backing table, but RETURNING from the view sees the original
        // NEW row and returns NULL for auto-assigned PKs). The paired
        // BEFORE INSERT guard trigger emitted in `rls.rs` keeps WITH
        // CHECK enforcement on this path. Plain INSERTs (no RETURNING)
        // keep going through the view's INSTEAD OF trigger.
        rewrite_rls_view_insert(&mut insert, schema, options);

        // Wrap text-literal values targeting `vector` / `halfvec` columns
        // with `vec_f32(...)` / `vec_f16(...)`. The main backing table is
        // BLOB STRICT, so a raw `'[0.1, 0.2, 0.3]'` text would otherwise
        // be rejected at apply time. Only direct VALUES rows are
        // rewritten. INSERT INTO ... SELECT carries arbitrary row shapes
        // through a subquery and is left untouched.
        wrap_vector_text_literals(&mut insert, schema);

        // Same shape for UUID-Blob columns: PG accepts text literals via
        // the `uuid` type's input function, but the translated BLOB
        // STRICT column does not. Wrap with the configured text-to-blob
        // expression (default `unhex(replace(literal, '-', ''))`).
        if is_blob_uuid_representation(options) {
            wrap_uuid_text_literals(&mut insert, schema, options)?;
        }

        if let Some(on_insert) = &self.on {
            match on_insert {
                sqlparser::ast::OnInsert::OnConflict(on_conflict) => {
                    match &on_conflict.action {
                        sqlparser::ast::OnConflictAction::DoNothing => {
                            // SQLite uses INSERT OR IGNORE
                            insert.or = Some(sqlparser::ast::SqliteOnConflict::Ignore);
                            insert.on = None;
                        }
                        sqlparser::ast::OnConflictAction::DoUpdate(do_update) => {
                            // The conflict target has to name columns before it
                            // reaches the shared translator, which copies it
                            // through.
                            let resolved = sqlparser::ast::OnConflict {
                                conflict_target: resolve_conflict_target(
                                    on_conflict.conflict_target.as_ref(),
                                    &insert.table,
                                    schema,
                                )?,
                                action: on_conflict.action.clone(),
                            };
                            insert.on = Some(translate_on_conflict_do_update::<Forward>(
                                &resolved, do_update, schema, options,
                            )?);
                        }
                    }
                }
                _ => {
                    return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
                        "Unsupported ON INSERT clause: {on_insert:?}"
                    )));
                }
            }
        }
        Ok(insert)
    }
}

/// Resolves `ON CONFLICT ON CONSTRAINT <name>` to the column list SQLite needs.
///
/// SQLite's conflict target is a column list, so the named form has to be
/// looked up rather than passed through, which emitted `near "ON": syntax
/// error`. Dropping the target is not an alternative: it changes which
/// conflicts the statement catches, and `DO UPDATE` with no target does not
/// parse in SQLite either.
///
/// A constraint that cannot be resolved is an error, because guessing a
/// different target would silently upsert on the wrong conflict.
fn resolve_conflict_target(
    conflict_target: Option<&sqlparser::ast::ConflictTarget>,
    table: &TableObject,
    schema: &ParserDB,
) -> Result<Option<sqlparser::ast::ConflictTarget>, crate::errors::Error> {
    let Some(sqlparser::ast::ConflictTarget::OnConstraint(constraint)) = conflict_target else {
        return Ok(conflict_target.cloned());
    };

    let wanted = last_ident_value_or_display(constraint);
    let TableObject::TableName(table_name) = table else {
        return Err(unresolvable_constraint(&wanted, &table.to_string()));
    };
    let Some(resolved_table) = table_with_implicit_public_lookup(schema, table_name)? else {
        return Err(unresolvable_constraint(&wanted, &table_name.to_string()));
    };

    for unique_index in resolved_table.unique_indices(schema)? {
        let columns: Vec<String> =
            unique_index.columns(schema)?.map(|column| column.column_name().to_owned()).collect();
        if columns.is_empty() {
            continue;
        }

        let declared = declared_constraint_name(unique_index.attribute());
        let generated = postgres_constraint_name(
            resolved_table.table_name(),
            &columns,
            unique_index.is_primary_key(schema)?,
        );

        if declared.is_some_and(|name| name.eq_ignore_ascii_case(&wanted))
            || generated.eq_ignore_ascii_case(&wanted)
        {
            return Ok(Some(sqlparser::ast::ConflictTarget::Columns(
                columns.into_iter().map(sqlparser::ast::Ident::new).collect(),
            )));
        }
    }

    Err(unresolvable_constraint(&wanted, &table_name.to_string()))
}

/// The name a unique constraint was declared with, or `None` when it was
/// anonymous.
///
/// Read off the constraint rather than through `IndexLike::name()`, which
/// looks like the accessor for this and always returns `None` for a unique
/// constraint: the declared name is an `Ident` while that accessor returns an
/// `ObjectName`, so `sql-traits` documents the omission rather than lying about
/// the shape. PostgreSQL spells the name `CONSTRAINT uq UNIQUE (..)` and MySQL
/// spells it `UNIQUE KEY uq (..)`, which `sqlparser` keeps in separate fields.
fn declared_constraint_name(constraint: &sqlparser::ast::UniqueConstraint) -> Option<&str> {
    constraint.name.as_ref().or(constraint.index_name.as_ref()).map(|name| name.value.as_str())
}

/// The name PostgreSQL gives an unnamed constraint, verified against
/// PostgreSQL 16 by reading `pg_constraint`: `<table>_pkey` for a primary key,
/// and `<table>_<column>_key` for a unique constraint with every column joined
/// by an underscore.
fn postgres_constraint_name(table: &str, columns: &[String], is_primary_key: bool) -> String {
    if is_primary_key {
        return format!("{table}_pkey");
    }
    format!("{table}_{}_key", columns.join("_"))
}

fn unresolvable_constraint(constraint: &str, table: &str) -> crate::errors::Error {
    crate::errors::Error::UnsupportedSQLiteFeature(format!(
        "ON CONFLICT ON CONSTRAINT {constraint} cannot be translated because {table} declares no \
         unique constraint of that name, and SQLite's conflict target is a column list. Name the \
         conflicting columns instead, as ON CONFLICT (col, ...)."
    ))
}

/// Replaces every `DEFAULT` in a `VALUES` row with the column's declared
/// default, since SQLite accepts the keyword only in `INSERT INTO t DEFAULT
/// VALUES` and rejects it inside a row with `near "DEFAULT": syntax error`.
///
/// Three outcomes per column, and the middle one is the case worth stating.
/// A declared default is translated and substituted. A column with no declared
/// default takes `NULL`, which is what PostgreSQL inserts too, and which for a
/// generated primary key is exactly right: PostgreSQL takes the next sequence
/// value while SQLite assigns the rowid, both from the same statement, verified
/// to yield 1 and 2 for two defaulted rows. A `NOT NULL` column with no
/// declared default is refused, because the insert could then only ever fail.
///
/// Unlike the vector and UUID walkers beside it this one reports rather than
/// returning quietly, since a `DEFAULT` left in place cannot execute. It
/// therefore checks for one FIRST: an insert carrying none must not be made to
/// resolve its target, which for a function-style table object cannot be
/// resolved at all.
fn substitute_default_values(
    insert: &mut Insert,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<(), crate::errors::Error> {
    if !carries_default(insert) {
        return Ok(());
    }

    let TableObject::TableName(table_name) = &insert.table else {
        return Err(default_without_a_named_table(&insert.table));
    };
    let Some(table) = table_with_implicit_public_lookup(schema, table_name)? else {
        return Err(unknown_default_table(table_name));
    };

    let column_names: Vec<String> = if insert.columns.is_empty() {
        table.columns(schema)?.map(|column| column.column_name().to_owned()).collect()
    } else {
        insert.columns.iter().filter_map(|n| last_ident(n).map(|i| i.value.clone())).collect()
    };

    let Some(source) = insert.source.as_deref_mut() else { return Ok(()) };
    let SetExpr::Values(values) = source.body.as_mut() else { return Ok(()) };

    for row in &mut values.rows {
        for (index, expr) in row.content.iter_mut().enumerate() {
            if !is_default_keyword(expr) {
                continue;
            }
            let column_name = column_names
                .get(index)
                .ok_or_else(|| default_without_a_column(table_name, index))?;
            *expr = default_expr_for_column(table, column_name, schema, options)?;
        }
    }

    Ok(())
}

/// The expression a `DEFAULT` in a `VALUES` row stands for.
fn default_expr_for_column(
    table: &<ParserDB as sql_traits::traits::DatabaseLike>::Table,
    column_name: &str,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<sqlparser::ast::Expr, crate::errors::Error> {
    let Some(column) = table.column(column_name, schema)? else {
        return Err(unknown_default_column(table.table_name(), column_name));
    };

    // Read the default off the column definition rather than through
    // `ColumnLike::default_value()`, which renders it back to a String that
    // would have to be reparsed. Returned untranslated on purpose: the caller
    // substitutes it before the source is translated, so a PostgreSQL default
    // such as `now()` goes through the ordinary expression path.
    for option in &column.attribute().options {
        if let sqlparser::ast::ColumnOption::Default(expr) = &option.option {
            return Ok(expr.clone());
        }
    }

    if column.is_nullable(schema)? || is_generated_primary_key(table, column_name, schema, options)?
    {
        return Ok(sqlparser::ast::Expr::Value(sqlparser::ast::ValueWithSpan {
            value: sqlparser::ast::Value::Null,
            span: sqlparser::tokenizer::Span::empty(),
        }));
    }

    Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
        "DEFAULT was written for {}.{column_name}, which declares no default and is NOT NULL, so \
         there is nothing to insert and the statement could only fail. Give the column a DEFAULT, \
         or write the value out.",
        table.table_name()
    )))
}

/// True when `column_name` is the whole primary key and holds an integer, which
/// SQLite translates to a rowid alias. Inserting `NULL` there assigns the next
/// rowid, so a PostgreSQL `SERIAL PRIMARY KEY` keeps generating values through
/// the same statement.
fn is_generated_primary_key(
    table: &<ParserDB as sql_traits::traits::DatabaseLike>::Table,
    column_name: &str,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<bool, crate::errors::Error> {
    let mut primary_key = table.primary_key_columns(schema)?;
    let Some(only) = primary_key.next() else { return Ok(false) };
    if primary_key.next().is_some() || !only.column_name().eq_ignore_ascii_case(column_name) {
        return Ok(false);
    }

    // Ask the data type translator rather than enumerating PostgreSQL spellings:
    // `SERIAL` reaches here as `DataType::Custom("serial")` and only the
    // translator knows it becomes `INTEGER`, which is what makes the column a
    // rowid alias.
    Ok(matches!(
        only.attribute().data_type.translate(schema, options)?,
        sqlparser::ast::DataType::Int(_)
            | sqlparser::ast::DataType::Integer(_)
            | sqlparser::ast::DataType::BigInt(_)
            | sqlparser::ast::DataType::SmallInt(_)
    ))
}

fn default_without_a_column(
    table: &sqlparser::ast::ObjectName,
    index: usize,
) -> crate::errors::Error {
    crate::errors::Error::UnsupportedSQLiteFeature(format!(
        "DEFAULT appears at position {} of a VALUES row for {table}, which names no column there, \
         so there is no default to substitute.",
        index + 1
    ))
}

/// True when any `VALUES` row of `insert` carries the bare `DEFAULT` keyword.
///
/// Checked before anything else so an insert that never mentions it is not made
/// to resolve its target, which a function-style table object cannot do.
fn carries_default(insert: &Insert) -> bool {
    let Some(source) = insert.source.as_deref() else { return false };
    let SetExpr::Values(values) = source.body.as_ref() else { return false };
    values.rows.iter().any(|row| row.content.iter().any(is_default_keyword))
}

fn default_without_a_named_table(table: &TableObject) -> crate::errors::Error {
    crate::errors::Error::UnsupportedSQLiteFeature(format!(
        "DEFAULT was written in a VALUES row for {table}, which is not a named table, so there are \
         no column defaults to resolve."
    ))
}

fn unknown_default_table(table: &sqlparser::ast::ObjectName) -> crate::errors::Error {
    crate::errors::Error::UnsupportedSQLiteFeature(format!(
        "DEFAULT was written in a VALUES row for {table}, which the translation schema does not \
         declare, so its column defaults cannot be resolved."
    ))
}

fn unknown_default_column(table: &str, column_name: &str) -> crate::errors::Error {
    crate::errors::Error::UnsupportedSQLiteFeature(format!(
        "DEFAULT was written for {table}.{column_name}, which the translation schema does not \
         declare, so its default cannot be resolved."
    ))
}

/// Rewrite each text-literal value at a vector-column position in a
/// `VALUES` source so that the BLOB STRICT main table accepts it. The
/// schema, table, or column shape can fail to resolve for many benign
/// reasons (target table not declared yet, table function source, INSERT
/// INTO ... SELECT); in those cases the function silently returns and
/// leaves the insert verbatim, which preserves the prior behaviour for
/// every non-vector path.
fn wrap_vector_text_literals(insert: &mut Insert, schema: &ParserDB) {
    let TableObject::TableName(table_name) = &insert.table else { return };
    let Ok(Some(table)) = table_with_implicit_public_lookup(schema, table_name) else {
        return;
    };
    // A table absent from the schema has no vector columns to wrap, so this
    // leaves the insert verbatim exactly as the lookup above does.
    let Ok(vector_cols) = vector_columns_of_table(table, schema) else { return };
    if vector_cols.is_empty() {
        return;
    }

    // Column order for matching values: the explicit list when present,
    // otherwise the natural table order. Comparison is
    // case-insensitive to match PostgreSQL's default identifier folding.
    let column_names: Vec<String> = if insert.columns.is_empty() {
        let Ok(columns) = table.columns(schema) else { return };
        columns.map(|c| c.column_name().to_string()).collect()
    } else {
        insert.columns.iter().filter_map(|n| last_ident(n).map(|i| i.value.clone())).collect()
    };

    let Some(source) = insert.source.as_deref_mut() else { return };
    let SetExpr::Values(values) = source.body.as_mut() else { return };

    for row in &mut values.rows {
        for (idx, expr) in row.content.iter_mut().enumerate() {
            let Some(col_name) = column_names.get(idx) else { break };
            if let Some((_, is_halfvec)) =
                vector_cols.iter().find(|(name, _)| name.eq_ignore_ascii_case(col_name))
            {
                let taken = core::mem::replace(
                    expr,
                    sqlparser::ast::Expr::Identifier(sqlparser::ast::Ident::new("__placeholder")),
                );
                *expr = maybe_wrap_text_vector_literal(taken, *is_halfvec);
            }
        }
    }
}

/// Redirect `INSERT INTO <view> ... RETURNING ...` to the backing table
/// when the view is an RLS translation of a PG table. The INSTEAD OF
/// view trigger does forward the INSERT, but the outer RETURNING reads
/// from the view's NEW row and never sees the rowid SQLite assigned in
/// the backing table. Pointing the INSERT at the backing table lets
/// RETURNING surface the correct values; policy enforcement is
/// preserved via the BEFORE INSERT guard trigger emitted by
/// `rls::generate_insert_check_trigger_sql`.
///
/// Scoped narrowly: only INSERTs that (a) target an RLS-enabled view
/// (looked up via [`rls::table_has_rls`]) and (b) carry a RETURNING
/// clause are rewritten. Plain INSERTs continue through the existing
/// INSTEAD OF view path so the behavioural change is opt-in via
/// RETURNING. Defensive lookups: any failure to resolve the target
/// table leaves the insert untouched.
fn rewrite_rls_view_insert(insert: &mut Insert, schema: &ParserDB, options: &Pg2SqliteOptions) {
    if insert.returning.is_none() {
        return;
    }
    // Symmetric gate with `generate_insert_check_trigger_sql` in
    // rls.rs: the rewrite is safe only when the backing-table guard
    // trigger is in place. The guard is emitted in strict mode; in
    // monitor mode we leave the INSERT pointing at the view so the
    // existing INSTEAD OF trigger keeps enforcing WITH CHECK. The
    // (existing) consequence is that RETURNING surfaces NULL for
    // auto-assigned PKs in monitor mode - call `with_strict_rls_validation()`
    // to unlock RETURNING through RLS views.
    if !options.is_strict_rls_validation() {
        return;
    }
    let TableObject::TableName(table_name) = &insert.table else { return };
    let Ok(Some(_table)) = table_with_implicit_public_lookup(schema, table_name) else {
        return;
    };
    let Some(last) = last_ident(table_name) else { return };
    let Ok(true) = rls::table_has_rls(&last.value, schema) else {
        return;
    };

    // Build the backing-table ObjectName: same schema prefix as the
    // original, but the bare table name gets the configured RLS
    // suffix (defaulting to "_rls").
    let suffix = options.get_rls_table_suffix();
    let backing_name = format!("{}{suffix}", last.value);
    let mut new_parts = table_name.0.clone();
    if let Some(last_part) = new_parts.last_mut() {
        *last_part =
            sqlparser::ast::ObjectNamePart::Identifier(sqlparser::ast::Ident::new(backing_name));
    }
    insert.table = TableObject::TableName(sqlparser::ast::ObjectName(new_parts));
}

/// Rewrite each text-literal value at a UUID-column position in a `VALUES`
/// source so that the BLOB STRICT main table accepts it. Same defensive
/// posture as `wrap_vector_text_literals`: silently skip on any lookup
/// failure (table not in schema, table function source,
/// INSERT INTO ... SELECT).
fn wrap_uuid_text_literals(
    insert: &mut Insert,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<(), crate::errors::Error> {
    let TableObject::TableName(table_name) = &insert.table else { return Ok(()) };
    let Ok(Some(table)) = table_with_implicit_public_lookup(schema, table_name) else {
        return Ok(());
    };
    // A table absent from the schema has no UUID columns to wrap, so this leaves
    // the insert verbatim exactly as the lookup above does.
    let Ok(uuid_cols) = uuid_columns_of_table(table, schema) else { return Ok(()) };
    if uuid_cols.is_empty() {
        return Ok(());
    }

    let column_names: Vec<String> = if insert.columns.is_empty() {
        let Ok(columns) = table.columns(schema) else { return Ok(()) };
        columns.map(|c| c.column_name().to_string()).collect()
    } else {
        insert.columns.iter().filter_map(|n| last_ident(n).map(|i| i.value.clone())).collect()
    };

    let Some(source) = insert.source.as_deref_mut() else { return Ok(()) };
    let SetExpr::Values(values) = source.body.as_mut() else { return Ok(()) };

    for row in &mut values.rows {
        for (idx, expr) in row.content.iter_mut().enumerate() {
            let Some(col_name) = column_names.get(idx) else { break };
            if uuid_cols.iter().any(|name| name.eq_ignore_ascii_case(col_name)) {
                let taken = core::mem::replace(
                    expr,
                    sqlparser::ast::Expr::Identifier(sqlparser::ast::Ident::new("__placeholder")),
                );
                *expr = maybe_wrap_text_uuid_literal(taken, options)?;
            }
        }
    }
    Ok(())
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use sql_traits::structs::ParserDB;
    use sqlparser::{
        ast::{Insert, OnInsert, Statement},
        dialect::PostgreSqlDialect,
        parser::Parser,
    };

    use crate::prelude::{Pg2SqliteOptions, Translator};

    fn empty_schema() -> ParserDB {
        ParserDB::from_statements(Vec::new(), "test".to_string()).expect("schema should build")
    }

    fn parse_insert(sql: &str) -> Insert {
        let stmt =
            Parser::parse_sql(&PostgreSqlDialect {}, sql).expect("sql should parse").remove(0);
        let Statement::Insert(insert) = stmt else {
            panic!("expected insert");
        };
        insert
    }

    #[test]
    fn translate_rejects_non_on_conflict_insert_clause() {
        let mut insert = parse_insert("INSERT INTO users(id) VALUES (1)");
        insert.on = Some(OnInsert::DuplicateKeyUpdate(Vec::new()));

        let schema = empty_schema();
        let options = Pg2SqliteOptions::default();
        let err = insert
            .translate(&schema, &options)
            .expect_err("non-on-conflict ON INSERT clause should fail");

        assert!(err.to_string().contains("Unsupported ON INSERT clause"));
    }
}
