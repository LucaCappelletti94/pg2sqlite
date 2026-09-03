//! Implementation of the [`Translator`](crate::traits::Translator) trait for
//! the `Insert` type.

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
    traits::{ColumnLike, DatabaseLike, IndexLike, TableLike, UniqueIndexLike},
};
use sqlparser::ast::{Insert, SelectItem, SetExpr, TableObject};

use super::helpers::Forward;
use crate::{
    impls::{
        object_name::{
            last_ident, last_ident_value_or_display,
            normalize_schema_qualified_object_name_for_sqlite, resolve_translation_table,
        },
        shared_helpers::{
            ColumnReferences, ColumnRewrites, carries_default_keyword, extract_columns_from_expr,
            is_default_keyword, scale_literal_for_column, substituted_assignment_default,
            translate_on_conflict_do_update, translate_returning,
        },
        translator_impls::{
            rls,
            uuid::{
                is_blob_uuid_representation, make_uuid_conversion_call,
                maybe_wrap_text_uuid_literal, uuid_columns_of_table,
            },
            vector::{maybe_wrap_text_vector_literal, vector_columns_of_table},
        },
    },
    traits::translator::TranslatorWithContext,
};
type ParserTable = <ParserDB as DatabaseLike>::Table;

struct ResolvedInsertTarget<'schema> {
    table: Option<&'schema ParserTable>,
    error: Option<crate::errors::Error>,
}

impl<'schema> ResolvedInsertTarget<'schema> {
    fn new(table: &TableObject, schema: &'schema ParserDB) -> Self {
        let TableObject::TableName(name) = table else {
            return Self { table: None, error: None };
        };
        match resolve_translation_table(schema, name) {
            Ok(table) => Self { table, error: None },
            Err(error) => Self { table: None, error: Some(error) },
        }
    }

    fn optional(&self) -> Option<&'schema ParserTable> {
        self.table
    }

    fn required(
        &mut self,
        missing: impl FnOnce() -> crate::errors::Error,
    ) -> Result<&'schema ParserTable, crate::errors::Error> {
        if let Some(error) = self.error.take() {
            return Err(error);
        }
        self.table.ok_or_else(missing)
    }
}

crate::traits::translator::impl_contextual_translator!(Insert => Insert);
/// The relation an `INSERT` names, whose columns a default, an `ON CONFLICT`
/// clause or a `RETURNING` item resolves against.
pub(crate) fn insert_target_scope<'db>(
    insert: &Insert,
    schema: &'db sql_traits::structs::ParserDB,
) -> Result<
    Option<sql_traits::structs::ColumnScope<'db, 'db, sql_traits::structs::ParserDB>>,
    crate::errors::Error,
> {
    let sqlparser::ast::TableObject::TableName(name) = &insert.table else {
        return Ok(None);
    };
    if crate::impls::object_name::last_ident(name).is_none() {
        return Ok(None);
    }
    Ok(crate::impls::object_name::resolve_translation_table(schema, name)?
        .map(|table| sql_traits::structs::ColumnScope::for_table(table, schema)))
}

impl crate::traits::translator::TranslatorWithContext for Insert {
    fn translate_with_warnings(
        &self,
        schema: &Self::Schema,
        options: &crate::options::TranslationContext<'_>,
        emit: &mut dyn FnMut(crate::warnings::TranslationWarning),
    ) -> Result<Self::SQLiteEntry, crate::errors::Error> {
        // Replace DEFAULT with the column's declared default BEFORE translating
        // the source, for two reasons. The substituted expression is PostgreSQL
        // SQL and gets translated by the same path as a written-out value
        // rather than needing its own call. And once no INSERT can carry the
        // keyword any further, `translate_values_rows` can refuse a DEFAULT
        // reaching it from anywhere else, which is the only other place the
        // parser accepts one.
        // The statement's target is the relation an unqualified column names,
        // and a query inside the statement attaches its own scope over this
        // one, so an outer reference still resolves.
        let target_scope = insert_target_scope(self, schema)?;
        let scoped = target_scope.as_ref().map(|scope| options.with_scope(scope));
        let options = scoped.as_ref().unwrap_or(options);
        let mut prepared = self.clone();
        substitute_default_values(&mut prepared, schema, options, emit)?;

        let source = prepared
            .source
            .as_ref()
            .map(|q| q.translate_with_warnings(schema, options, emit))
            .transpose()?
            .map(Box::new);

        let returning =
            translate_returning::<Forward>(self.returning.as_ref(), schema, options, emit)?;

        let mut insert = Insert { source, returning, ..prepared };

        // INSERT INTO <RLS view> ... RETURNING ...: rewrite to target
        // the backing table so RETURNING surfaces the row that was
        // actually written (the INSTEAD OF view trigger writes to the
        // backing table, but RETURNING from the view sees the original
        // NEW row and returns NULL for auto-assigned PKs). The paired
        // BEFORE INSERT guard trigger emitted in `rls.rs` keeps WITH
        // CHECK enforcement on this path. Plain INSERTs (no RETURNING)
        // keep going through the view's INSTEAD OF trigger.
        let mut target = rewrite_rls_view_insert(&mut insert, schema, options, emit)?;

        // Wrap text-literal values targeting `vector` / `halfvec` columns
        // with `vec_f32(...)` / `vec_f16(...)`. The main backing table is
        // BLOB STRICT, so a raw `'[0.1, 0.2, 0.3]'` text would otherwise
        // be rejected at apply time. Only direct VALUES rows are
        // rewritten. INSERT INTO ... SELECT carries arbitrary row shapes
        // through a subquery and is left untouched.
        wrap_vector_text_literals(&mut insert, target.optional(), schema);

        // Same shape for UUID-Blob columns: PG accepts text literals via
        // the `uuid` type's input function, but the translated BLOB
        // STRICT column does not. Wrap with the configured text-to-blob
        // expression (default `unhex(replace(literal, '-', ''))`).
        if is_blob_uuid_representation(options) {
            wrap_uuid_text_literals(&mut insert, target.optional(), schema, options)?;
        }

        // A NUMERIC column is an INTEGER of minor units, so a decimal literal
        // has to be moved onto that scale before it reaches a STRICT table.
        // The full rewrite set serves the DO UPDATE list below, which writes
        // into the same columns the insert does.
        let rewrites = target.optional().map_or_else(ColumnRewrites::default, |table| {
            ColumnRewrites::of_table(table, schema, options)
        });
        scale_numeric_literals(&mut insert, target.optional(), schema, &rewrites.numeric_scales)?;

        if let Some(on_insert) = &self.on {
            match on_insert {
                sqlparser::ast::OnInsert::OnConflict(on_conflict) => {
                    match &on_conflict.action {
                        sqlparser::ast::OnConflictAction::DoNothing => {
                            // PostgreSQL suppresses a conflict on the arbiter
                            // index and nothing else, which is exactly what
                            // SQLite's upsert clause does. `INSERT OR IGNORE`
                            // suppresses every constraint failure the
                            // statement can raise, so a CHECK or NOT NULL
                            // violation PostgreSQL reports became a silently
                            // skipped row. The target needs the same lookup
                            // `DO UPDATE` gives it, since it is no longer
                            // discarded.
                            insert.on = Some(sqlparser::ast::OnInsert::OnConflict(
                                sqlparser::ast::OnConflict {
                                    conflict_target: resolve_conflict_target(
                                        on_conflict.conflict_target.as_ref(),
                                        &insert.table,
                                        &mut target,
                                        schema,
                                    )?,
                                    action: sqlparser::ast::OnConflictAction::DoNothing,
                                },
                            ));
                        }
                        sqlparser::ast::OnConflictAction::DoUpdate(do_update) => {
                            // The conflict target has to name columns before it
                            // reaches the shared translator, which copies it
                            // through.
                            let resolved = sqlparser::ast::OnConflict {
                                conflict_target: resolve_conflict_target(
                                    on_conflict.conflict_target.as_ref(),
                                    &insert.table,
                                    &mut target,
                                    schema,
                                )?,
                                action: on_conflict.action.clone(),
                            };
                            // PostgreSQL accepts DEFAULT in a DO UPDATE list
                            // and stores the declared default, so it is
                            // substituted here, where the target table is in
                            // hand, before the shared translator runs.
                            let substituted = substitute_do_update_defaults(
                                do_update,
                                &insert.table,
                                &mut target,
                                schema,
                                options,
                                emit,
                            )?;
                            let do_update = substituted.as_ref().unwrap_or(do_update);
                            insert.on = Some(translate_on_conflict_do_update::<Forward>(
                                &resolved, do_update, schema, options, &rewrites, emit,
                            )?);
                        }
                    }
                }
                _ => {
                    return Err(crate::errors::Error::forward_refusal(format!(
                        "Unsupported ON INSERT clause: {on_insert:?}"
                    )));
                }
            }
        }

        if matches!(insert.on, Some(sqlparser::ast::OnInsert::OnConflict(_))) {
            disambiguate_upsert_source(&mut insert);
        }

        // SQLite has no schemas, and a qualified name on a write inside a
        // trigger body is a parse error at CREATE TRIGGER, so the emitted
        // target carries the object part alone. This runs last because every
        // lookup above resolves the name the schema declared.
        if let TableObject::TableName(name) = &insert.table {
            insert.table = TableObject::TableName(
                normalize_schema_qualified_object_name_for_sqlite(schema, name)?,
            );
        }

        Ok(insert)
    }
}

/// Gives a bare `SELECT` source the `WHERE` clause SQLite needs to tell an
/// upsert clause apart from the tail of the select.
///
/// This is the spelling SQLite's grammar requires, not a way around it: the
/// ambiguity is in SQLite's own parser, and its documentation states that a
/// select feeding an upsert must carry a `WHERE` even when the condition is
/// trivial. Without one, `INSERT INTO t (a) SELECT a FROM u ON CONFLICT (a)
/// DO NOTHING` answers `near "DO": syntax error`. Measured on 3.46.0, a
/// source ending in `WHERE`, `GROUP BY`, `ORDER BY` or `LIMIT`, a set
/// operation, and a `VALUES` list all parse unaided, so only the bare form is
/// touched, and `WHERE true` selects every row it is added to.
fn disambiguate_upsert_source(insert: &mut Insert) {
    let Some(source) = insert.source.as_deref_mut() else { return };
    let SetExpr::Select(select) = source.body.as_mut() else { return };
    if select.selection.is_some() {
        return;
    }
    select.selection = Some(sqlparser::ast::Expr::Value(sqlparser::ast::ValueWithSpan {
        value: sqlparser::ast::Value::Boolean(true),
        span: sqlparser::tokenizer::Span::empty(),
    }));
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
    target: &mut ResolvedInsertTarget<'_>,
    schema: &ParserDB,
) -> Result<Option<sqlparser::ast::ConflictTarget>, crate::errors::Error> {
    let Some(sqlparser::ast::ConflictTarget::OnConstraint(constraint)) = conflict_target else {
        return Ok(conflict_target.cloned());
    };

    let wanted = last_ident_value_or_display(constraint);
    let TableObject::TableName(table_name) = table else {
        return Err(unresolvable_constraint(&wanted, &table.to_string()));
    };
    let resolved_table =
        target.required(|| unresolvable_constraint(&wanted, &table_name.to_string()))?;

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
    crate::errors::Error::forward_refusal(format!(
        "ON CONFLICT ON CONSTRAINT {constraint} cannot be translated because {table} declares no \
         unique constraint of that name, and SQLite's conflict target is a column list. Name the \
         conflicting columns instead, as ON CONFLICT (col, ...)."
    ))
}

/// Substitutes declared defaults into a DO UPDATE assignment list, or answers
/// `None` when no assignment carries the `DEFAULT` keyword.
///
/// PostgreSQL accepts the keyword there and stores the declared default,
/// measured on PostgreSQL 16. The substitution happens before the shared
/// translator runs, so the raw PostgreSQL default flows through the ordinary
/// translate and finish pipeline like any written value.
fn substitute_do_update_defaults(
    do_update: &sqlparser::ast::DoUpdate,
    table_object: &TableObject,
    target: &mut ResolvedInsertTarget<'_>,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Option<sqlparser::ast::DoUpdate>, crate::errors::Error> {
    if !do_update.assignments.iter().any(|a| carries_default_keyword(&a.value)) {
        return Ok(None);
    }
    let TableObject::TableName(table_name) = table_object else {
        return Err(default_without_a_named_table(table_object));
    };
    let table = target.required(|| unknown_default_table(table_name))?;

    let assignments = do_update
        .assignments
        .iter()
        .map(|a| {
            let substituted =
                substituted_assignment_default(&a.target, &a.value, table, schema, options, emit)?;
            Ok(sqlparser::ast::Assignment {
                target: a.target.clone(),
                value: substituted.unwrap_or_else(|| a.value.clone()),
            })
        })
        .collect::<Result<Vec<_>, crate::errors::Error>>()?;

    Ok(Some(sqlparser::ast::DoUpdate { assignments, selection: do_update.selection.clone() }))
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
    options: &crate::options::TranslationContext<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<(), crate::errors::Error> {
    if !carries_default(insert) {
        return Ok(());
    }

    let TableObject::TableName(table_name) = &insert.table else {
        return Err(default_without_a_named_table(&insert.table));
    };
    let Some(table) = resolve_translation_table(schema, table_name)? else {
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
            *expr = default_expr_for_column(table, column_name, schema, options, emit)?;
        }
    }

    Ok(())
}

/// The expression a `DEFAULT` in a `VALUES` row stands for.
pub(crate) fn default_expr_for_column(
    table: &<ParserDB as sql_traits::traits::DatabaseLike>::Table,
    column_name: &str,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
    emit: crate::warnings::WarningSink<'_>,
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

    if column.is_nullable(schema)?
        || is_generated_primary_key(table, column_name, schema, options, emit)?
    {
        return Ok(sqlparser::ast::Expr::Value(sqlparser::ast::ValueWithSpan {
            value: sqlparser::ast::Value::Null,
            span: sqlparser::tokenizer::Span::empty(),
        }));
    }

    Err(crate::errors::Error::forward_refusal(format!(
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
    options: &crate::options::TranslationContext<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<bool, crate::errors::Error> {
    let mut primary_key = table.primary_key_columns(schema)?;
    let Some(only) = primary_key.next() else { return Ok(false) };
    if primary_key.next().is_some() || !only.column_name().eq_ignore_ascii_case(column_name) {
        return Ok(false);
    }

    // Ask the data type translator rather than enumerating PostgreSQL
    // spellings: `SERIAL` reaches here as `DataType::Custom("serial")` and
    // only the translator knows it becomes `INTEGER`, which is what makes
    // the column a rowid alias.
    Ok(matches!(
        only.attribute().data_type.translate_with_warnings(schema, options, emit)?,
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
    crate::errors::Error::forward_refusal(format!(
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
    crate::errors::Error::forward_refusal(format!(
        "DEFAULT was written in a VALUES row for {table}, which is not a named table, so there are \
         no column defaults to resolve."
    ))
}

fn unknown_default_table(table: &sqlparser::ast::ObjectName) -> crate::errors::Error {
    crate::errors::Error::forward_refusal(format!(
        "DEFAULT was written in a VALUES row for {table}, which the translation schema does not \
         declare, so its column defaults cannot be resolved."
    ))
}

fn unknown_default_column(table: &str, column_name: &str) -> crate::errors::Error {
    crate::errors::Error::forward_refusal(format!(
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
fn wrap_vector_text_literals(insert: &mut Insert, table: Option<&ParserTable>, schema: &ParserDB) {
    let Some(table) = table else { return };
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

/// Rewrite every decimal literal targeting a `NUMERIC` column as the integer
/// count of minor units the column now holds.
///
/// Bails out on the same unresolvable shapes as its vector and UUID siblings,
/// leaving the insert verbatim. Both source forms are handled: a VALUES row
/// and a SELECT projection map position to target column the same way.
fn scale_numeric_literals(
    insert: &mut Insert,
    table: Option<&ParserTable>,
    schema: &ParserDB,
    scales: &[(String, u32)],
) -> Result<(), crate::errors::Error> {
    if scales.is_empty() {
        return Ok(());
    }
    let Some(table) = table else { return Ok(()) };

    let column_names: Vec<String> = if insert.columns.is_empty() {
        let Ok(columns) = table.columns(schema) else { return Ok(()) };
        columns.map(|c| c.column_name().to_string()).collect()
    } else {
        insert.columns.iter().filter_map(|n| last_ident(n).map(|i| i.value.clone())).collect()
    };

    let Some(source) = insert.source.as_deref_mut() else { return Ok(()) };
    scale_insert_source_body(source.body.as_mut(), &column_names, scales)
}

/// Applies the D1 scaling to one arm of an insert source, recursing through
/// set operations and nested parentheses.
///
/// A VALUES row and a SELECT projection map position to target column the
/// same way, and every arm of a set operation feeds the same columns. Only a
/// literal projection is rewritten, aliased or bare: a projected column is
/// already in minor units and a computed projection cannot be scaled without
/// guessing, and a projection list containing a wildcard cannot be mapped
/// positionally, so those pass through. A fractional literal that survives
/// unscaled fails loudly on the STRICT table rather than storing a wrong
/// number.
fn scale_insert_source_body(
    body: &mut SetExpr,
    column_names: &[String],
    scales: &[(String, u32)],
) -> Result<(), crate::errors::Error> {
    match body {
        SetExpr::Values(values) => {
            for row in &mut values.rows {
                for (index, expr) in row.content.iter_mut().enumerate() {
                    let Some(column) = column_names.get(index) else { break };
                    scale_literal_for_column(expr, column, scales)?;
                }
            }
        }
        SetExpr::Select(select) => {
            // A wildcard expands to an unknown count and Spark's
            // `expr AS (a, b)` expands one item to several columns, so
            // neither projection list can be mapped positionally.
            if select.projection.iter().any(|item| {
                matches!(
                    item,
                    SelectItem::Wildcard(_)
                        | SelectItem::QualifiedWildcard(..)
                        | SelectItem::ExprWithAliases { .. }
                )
            }) {
                return Ok(());
            }
            for (index, item) in select.projection.iter_mut().enumerate() {
                let Some(column) = column_names.get(index) else { break };
                match item {
                    SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. } => {
                        scale_literal_for_column(expr, column, scales)?;
                    }
                    SelectItem::Wildcard(_)
                    | SelectItem::QualifiedWildcard(..)
                    | SelectItem::ExprWithAliases { .. } => {}
                }
            }
        }
        SetExpr::Query(query) => {
            scale_insert_source_body(query.body.as_mut(), column_names, scales)?;
        }
        SetExpr::SetOperation { left, right, .. } => {
            scale_insert_source_body(left, column_names, scales)?;
            scale_insert_source_body(right, column_names, scales)?;
        }
        _ => {}
    }
    Ok(())
}

/// Redirect an INSERT against a policy-bearing RLS view when it carries a
/// clause SQLite cannot handle on a view: a `RETURNING` clause or an
/// `ON CONFLICT` upsert clause.
///
/// **RETURNING:** The INSTEAD OF INSERT trigger forwards the row to the
/// backing table, but `RETURNING` reads from the view's NEW row and never
/// sees the rowid the backing table assigned. Redirecting the INSERT at the
/// backing table lets `RETURNING` surface the correct values.
///
/// **ON CONFLICT (DO NOTHING / DO UPDATE):** SQLite refuses these forms on a
/// view entirely ("cannot UPSERT a view"). `INSERT OR IGNORE` and
/// `INSERT OR REPLACE` use a different AST field and are NOT touched here.
///
/// In both cases the redirect is safe only in strict mode, because the
/// backing-table BEFORE INSERT guard (`generate_insert_check_trigger_sql`)
/// is emitted only then. Default mode logs rather than blocks, so
/// redirecting there would write past the policy.
///
/// Non-RLS tables and unresolvable targets are left untouched.
fn rewrite_rls_view_insert<'schema>(
    insert: &mut Insert,
    schema: &'schema ParserDB,
    options: &crate::options::TranslationContext<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<ResolvedInsertTarget<'schema>, crate::errors::Error> {
    let target = ResolvedInsertTarget::new(&insert.table, schema);
    // INSERT OR IGNORE / OR REPLACE use insert.or, not insert.on; they are
    // accepted by SQLite on a view and must not be redirected here.
    let has_on_conflict = matches!(&insert.on, Some(sqlparser::ast::OnInsert::OnConflict(_)));
    let returning = insert.returning.clone();

    if returning.is_none() && !has_on_conflict {
        return Ok(target);
    }

    let TableObject::TableName(table_name) = &insert.table else { return Ok(target) };
    let Some(table) = target.optional() else {
        return Ok(target);
    };
    let Some(last) = last_ident(table_name) else { return Ok(target) };
    let Ok(true) = rls::table_has_rls(&last.value, schema) else {
        return Ok(target);
    };

    if !options.is_strict_rls_validation() {
        // RETURNING: refuse when a database-filled column would come back NULL
        // from the view row.
        if let Some(items) = &returning
            && let Some(column) = database_filled_column(items, table, schema, options, emit)?
        {
            return Err(crate::errors::Error::forward_refusal(format!(
                "RETURNING reads {column} back from a view over {}, and a view row holds only what \
             the caller wrote, so a column the database fills in would come back NULL. Call \
             `with_strict_rls_validation()`, which sends a RETURNING insert straight at the \
             backing table, or write {column} out in the insert.",
                last.value
            )));
        }
        // ON CONFLICT: refuse, because no backing-table guard is emitted in
        // default mode and a redirect would write past the policy.
        if has_on_conflict {
            return Err(crate::errors::Error::forward_refusal(format!(
                "ON CONFLICT against {}, a policy-bearing table, cannot be forwarded to its view \
             because SQLite does not support UPSERT on a view. Call \
             `with_strict_rls_validation()` to redirect the insert to the backing table.",
                last.value
            )));
        }
        return Ok(target);
    }

    // Strict mode: redirect to the backing table. The BEFORE INSERT guard on
    // the backing table keeps WITH CHECK enforcement on this path.
    let suffix = options.get_rls_table_suffix();
    let backing_name = format!("{}{suffix}", last.value);
    let mut new_parts = table_name.0.clone();
    if let Some(last_part) = new_parts.last_mut() {
        *last_part =
            sqlparser::ast::ObjectNamePart::Identifier(sqlparser::ast::Ident::new(backing_name));
    }
    insert.table = TableObject::TableName(sqlparser::ast::ObjectName(new_parts));
    Ok(target)
}

/// The first returned column whose value the database supplies rather than the
/// caller: a declared default, a generated column, or the integer primary key
/// SQLite assigns. Those are exactly the columns a view cannot answer for.
///
/// # Errors
///
/// Propagates a schema lookup failure.
fn database_filled_column(
    returning: &[SelectItem],
    table: &<ParserDB as sql_traits::traits::DatabaseLike>::Table,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Option<String>, crate::errors::Error> {
    let every_column = || -> Result<Vec<String>, crate::errors::Error> {
        Ok(table.columns(schema)?.map(|column| column.column_name().to_owned()).collect())
    };

    let mut named = Vec::new();
    for item in returning {
        match item {
            SelectItem::UnnamedExpr(expr)
            | SelectItem::ExprWithAlias { expr, .. }
            | SelectItem::ExprWithAliases { expr, .. } => {
                match extract_columns_from_expr(expr) {
                    ColumnReferences::Complete(columns) => named.extend(columns),
                    ColumnReferences::Unknown => named.extend(every_column()?),
                }
            }
            SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(..) => {
                named.extend(every_column()?);
            }
        }
    }

    for name in named {
        let Some(column) = table.column(&name, schema)? else { continue };
        let filled = column.attribute().options.iter().any(|option| {
            matches!(
                option.option,
                sqlparser::ast::ColumnOption::Default(_)
                    | sqlparser::ast::ColumnOption::Generated { generation_expr: Some(_), .. }
            )
        }) || is_generated_primary_key(table, &name, schema, options, emit)?;
        if filled {
            return Ok(Some(name));
        }
    }

    Ok(None)
}

/// Rewrite each text-literal value at a UUID-column position in a `VALUES`
/// source so that the BLOB STRICT main table accepts it. Same defensive
/// posture as `wrap_vector_text_literals`: silently skip on any lookup
/// failure (table not in schema, table function source,
/// INSERT INTO ... SELECT).
fn wrap_uuid_text_literals(
    insert: &mut Insert,
    table: Option<&ParserTable>,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<(), crate::errors::Error> {
    let Some(table) = table else { return Ok(()) };
    // A table absent from the schema has no UUID columns to wrap, so this
    // leaves the insert verbatim exactly as the lookup above does.
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
                // Runtime placeholders (e.g. ?1 from $1) cannot be validated
                // at translation time. Wrap them in the brace-stripping unhex
                // call so braced and plain text binds both land as the 16-byte
                // blob the STRICT column requires.
                *expr = if matches!(
                    &taken,
                    sqlparser::ast::Expr::Value(sqlparser::ast::ValueWithSpan {
                        value: sqlparser::ast::Value::Placeholder(_),
                        ..
                    })
                ) {
                    make_uuid_conversion_call(taken, options)
                } else {
                    maybe_wrap_text_uuid_literal(taken, options)?
                };
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
