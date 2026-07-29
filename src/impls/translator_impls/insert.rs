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
    traits::{ColumnLike, TableLike},
};
use sqlparser::ast::{Insert, SetExpr, TableObject};

use super::helpers::Forward;
use crate::{
    impls::{
        object_name::{last_ident, table_with_implicit_public_lookup},
        shared_helpers::{translate_on_conflict_do_update, translate_returning},
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
        let source =
            self.source.as_ref().map(|q| q.translate(schema, options)).transpose()?.map(Box::new);

        let returning = translate_returning::<Forward>(self.returning.as_ref(), schema, options)?;

        let mut insert = Insert { source, returning, ..self.clone() };

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
            wrap_uuid_text_literals(&mut insert, schema, options);
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
                            insert.on = Some(translate_on_conflict_do_update::<Forward>(
                                on_conflict,
                                do_update,
                                schema,
                                options,
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
    let vector_cols = vector_columns_of_table(table, schema);
    if vector_cols.is_empty() {
        return;
    }

    // Column order for matching values: the explicit list when present,
    // otherwise the natural table order. Comparison is
    // case-insensitive to match PostgreSQL's default identifier folding.
    let column_names: Vec<String> = if insert.columns.is_empty() {
        table.columns(schema).map(|c| c.column_name().to_string()).collect()
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
    if !rls::table_has_rls(&last.value, schema) {
        return;
    }

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
fn wrap_uuid_text_literals(insert: &mut Insert, schema: &ParserDB, options: &Pg2SqliteOptions) {
    let TableObject::TableName(table_name) = &insert.table else { return };
    let Ok(Some(table)) = table_with_implicit_public_lookup(schema, table_name) else {
        return;
    };
    let uuid_cols = uuid_columns_of_table(table, schema);
    if uuid_cols.is_empty() {
        return;
    }

    let column_names: Vec<String> = if insert.columns.is_empty() {
        table.columns(schema).map(|c| c.column_name().to_string()).collect()
    } else {
        insert.columns.iter().filter_map(|n| last_ident(n).map(|i| i.value.clone())).collect()
    };

    let Some(source) = insert.source.as_deref_mut() else { return };
    let SetExpr::Values(values) = source.body.as_mut() else { return };

    for row in &mut values.rows {
        for (idx, expr) in row.content.iter_mut().enumerate() {
            let Some(col_name) = column_names.get(idx) else { break };
            if uuid_cols.iter().any(|name| name.eq_ignore_ascii_case(col_name)) {
                let taken = core::mem::replace(
                    expr,
                    sqlparser::ast::Expr::Identifier(sqlparser::ast::Ident::new("__placeholder")),
                );
                *expr = maybe_wrap_text_uuid_literal(taken, options);
            }
        }
    }
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
