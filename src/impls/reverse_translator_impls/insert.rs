//! Implementation of the [`ReverseTranslator`] trait for the
//! `Insert` type.

use std::collections::BTreeSet;

use sql_traits::{
    structs::ParserDB,
    traits::{ColumnLike, DatabaseLike, TableLike},
};
use sqlparser::ast::{
    Assignment, AssignmentTarget, ConflictTarget, DoUpdate, Expr, Ident, Insert, ObjectName,
    ObjectNamePart, OnConflict, OnConflictAction, OnInsert, SqliteOnConflict, TableObject,
};

use super::helpers::Reverse;
use crate::{
    errors::Error,
    impls::{
        object_name::{implicit_public_lookup_parts, table_with_implicit_public_lookup},
        shared_helpers::{translate_on_conflict_do_update, translate_returning},
    },
    prelude::{Pg2SqliteOptions, ReverseTranslator},
};

/// Resolve the target table for reverse upsert reconstruction.
///
/// Behavior:
/// - Accept unqualified or schema-qualified names with at most two parts.
/// - Try implicit-public lookup (`None`/`public`) first.
/// - For explicit non-public schemas, require schema resolution in `schema`.
/// - If still missing, scan all schemas for unique table-name match.
/// - If multiple schema matches exist, return an ambiguity error.
fn resolve_insert_table<'a>(
    schema: &'a ParserDB,
    table: &TableObject,
) -> Result<&'a <ParserDB as DatabaseLike>::Table, Error> {
    let TableObject::TableName(table_name) = table else {
        return Err(Error::UnsupportedSQLiteFeature(
            "INSERT OR REPLACE with table function is not supported".to_string(),
        ));
    };

    let (primary_schema, _, bare_table_name) = implicit_public_lookup_parts(table_name)?;
    let is_unqualified = primary_schema.is_none();

    if let Some(found) = table_with_implicit_public_lookup(schema, table_name)? {
        return Ok(found);
    }
    if !is_unqualified {
        return Err(Error::TableNotFoundInSchema { table_name: table_name.to_string() });
    }

    let bare_table_name = bare_table_name.as_ref();
    let candidates = schema
        .tables()
        .filter(|candidate| candidate.table_name().eq_ignore_ascii_case(bare_table_name))
        .collect::<Vec<_>>();

    match candidates.as_slice() {
        [] => Err(Error::TableNotFoundInSchema { table_name: bare_table_name.to_string() }),
        [single] => Ok(*single),
        many => {
            let schemas = many
                .iter()
                .map(|table| table.table_schema().unwrap_or("<default>").to_string())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            Err(Error::AmbiguousTableInSchema { table_name: bare_table_name.to_string(), schemas })
        }
    }
}

/// Look up primary key columns for a resolved table.
fn get_primary_key_columns(
    schema: &ParserDB,
    table: &<ParserDB as DatabaseLike>::Table,
) -> Vec<String> {
    table.primary_key_columns(schema).map(|c| c.column_name().to_string()).collect()
}

/// Resolve the INSERT column list.
///
/// SQLite allows `INSERT ... VALUES (...)` without an explicit column list.
/// For upsert reconstruction, we treat that form as "all table columns in
/// schema order".
fn resolve_insert_columns(
    schema: &ParserDB,
    table: &<ParserDB as DatabaseLike>::Table,
    explicit_columns: &[Ident],
) -> Vec<Ident> {
    if !explicit_columns.is_empty() {
        return explicit_columns.to_vec();
    }

    table.columns(schema).map(|column| Ident::new(column.column_name().to_string())).collect()
}

/// Build ON CONFLICT DO UPDATE SET clause for all non-PK columns.
/// Returns an error if the insert columns don't include all PK columns.
fn build_upsert_on_conflict(
    table_name: &str,
    pk_columns: &[String],
    insert_columns: &[Ident],
) -> Result<OnInsert, Error> {
    if pk_columns.is_empty() {
        return Err(Error::MissingPrimaryKeyInUpsert {
            table_name: table_name.to_string(),
            pk_columns: Vec::new(),
            insert_columns: insert_columns.iter().map(|c| c.value.clone()).collect(),
        });
    }

    // Validate that all PK columns are present in insert_columns
    let has_missing_pk = pk_columns
        .iter()
        .any(|pk| !insert_columns.iter().any(|ic| ic.value.eq_ignore_ascii_case(pk)));

    if has_missing_pk {
        return Err(Error::MissingPrimaryKeyInUpsert {
            table_name: table_name.to_string(),
            pk_columns: pk_columns.to_vec(),
            insert_columns: insert_columns.iter().map(|c| c.value.clone()).collect(),
        });
    }

    // Build conflict target from primary key columns
    let conflict_target =
        ConflictTarget::Columns(pk_columns.iter().map(|c| Ident::new(c.clone())).collect());

    // Build SET assignments for non-PK columns
    let assignments: Vec<Assignment> = insert_columns
        .iter()
        .filter(|col| !pk_columns.iter().any(|pk| pk.eq_ignore_ascii_case(&col.value)))
        .map(|col| {
            Assignment {
                target: AssignmentTarget::ColumnName(ObjectName(vec![ObjectNamePart::Identifier(
                    col.clone(),
                )])),
                value: Expr::CompoundIdentifier(vec![Ident::new("EXCLUDED"), col.clone()]),
            }
        })
        .collect();

    // PK-only tables have no non-PK columns to update; fall back to DO NOTHING
    // to avoid generating an empty (invalid) DO UPDATE SET clause.
    if assignments.is_empty() {
        return Ok(OnInsert::OnConflict(OnConflict {
            conflict_target: Some(conflict_target),
            action: OnConflictAction::DoNothing,
        }));
    }

    Ok(OnInsert::OnConflict(OnConflict {
        conflict_target: Some(conflict_target),
        action: OnConflictAction::DoUpdate(DoUpdate { assignments, selection: None }),
    }))
}

impl ReverseTranslator for Insert {
    type Schema = ParserDB;
    type Options = Pg2SqliteOptions;
    type PostgresEntry = Insert;

    #[allow(clippy::too_many_lines)]
    fn reverse_translate(
        &self,
        schema: &Self::Schema,
        options: &Self::Options,
    ) -> Result<Self::PostgresEntry, Error> {
        // Reverse translate the source (VALUES or SELECT)
        let source = self
            .source
            .as_ref()
            .map(|q| q.reverse_translate(schema, options))
            .transpose()?
            .map(Box::new);

        // Reverse translate partitioned expressions if present
        let partitioned = self
            .partitioned
            .as_ref()
            .map(|exprs| {
                exprs
                    .iter()
                    .map(|e| e.reverse_translate(schema, options))
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?;

        // Reverse translate RETURNING clause if present
        let returning = translate_returning::<Reverse>(self.returning.as_ref(), schema, options)?;

        // Reverse translate assignments if present
        let assignments = self
            .assignments
            .iter()
            .map(|assignment| {
                Ok(sqlparser::ast::Assignment {
                    target: assignment.target.clone(),
                    value: assignment.value.reverse_translate(schema, options)?,
                })
            })
            .collect::<Result<Vec<_>, Error>>()?;

        let table = match &self.table {
            TableObject::TableName(_) | TableObject::TableQuery(_) => self.table.clone(),
            TableObject::TableFunction(func) => {
                match super::function::reverse_translate_function(func, schema, options)? {
                    Expr::Function(f) => TableObject::TableFunction(f),
                    _ => self.table.clone(),
                }
            }
        };

        let mut insert = Insert {
            insert_token: self.insert_token.clone(),
            optimizer_hints: self.optimizer_hints.clone(),
            or: None, // Will be set below based on conversion
            ignore: self.ignore,
            into: self.into,
            table,
            table_alias: self.table_alias.clone(),
            columns: self.columns.clone(),
            overwrite: self.overwrite,
            source,
            assignments,
            partitioned,
            after_columns: self.after_columns.clone(),
            has_table_keyword: self.has_table_keyword,
            on: self.on.clone(),
            returning,
            output: self.output.clone(),
            replace_into: self.replace_into,
            priority: self.priority,
            insert_alias: self.insert_alias.clone(),
            settings: self.settings.clone(),
            format_clause: self.format_clause.clone(),
            multi_table_insert_type: self.multi_table_insert_type.clone(),
            multi_table_into_clauses: self.multi_table_into_clauses.clone(),
            multi_table_when_clauses: self.multi_table_when_clauses.clone(),
            multi_table_else_clause: self.multi_table_else_clause.clone(),
        };

        // Handle SQLite's INSERT OR IGNORE/REPLACE → PostgreSQL ON CONFLICT
        if let Some(or_clause) = &self.or {
            match or_clause {
                SqliteOnConflict::Ignore => {
                    // INSERT OR IGNORE → INSERT ... ON CONFLICT DO NOTHING
                    insert.on = Some(OnInsert::OnConflict(OnConflict {
                        conflict_target: None,
                        action: OnConflictAction::DoNothing,
                    }));
                }
                SqliteOnConflict::Replace => {
                    // INSERT OR REPLACE → INSERT ... ON CONFLICT (pk) DO UPDATE SET ...
                    // Look up the primary key from the schema
                    let resolved_table = resolve_insert_table(schema, &self.table)?;
                    let table_name = resolved_table.table_name().to_string();
                    let pk_columns = get_primary_key_columns(schema, resolved_table);
                    let column_idents: Vec<Ident> = self
                        .columns
                        .iter()
                        .filter_map(|c| {
                            c.0.last().and_then(sqlparser::ast::ObjectNamePart::as_ident).cloned()
                        })
                        .collect();
                    let insert_columns =
                        resolve_insert_columns(schema, resolved_table, &column_idents);
                    insert.on =
                        Some(build_upsert_on_conflict(&table_name, &pk_columns, &insert_columns)?);
                }
                SqliteOnConflict::Rollback | SqliteOnConflict::Abort | SqliteOnConflict::Fail => {
                    // These don't have direct PostgreSQL equivalents
                    // Leave on as-is (from above)
                }
            }
        }

        // Reverse translate ON CONFLICT expressions if present
        if let Some(OnInsert::OnConflict(on_conflict)) = &self.on
            && let OnConflictAction::DoUpdate(do_update) = &on_conflict.action
        {
            insert.on = Some(translate_on_conflict_do_update::<Reverse>(
                on_conflict,
                do_update,
                schema,
                options,
            )?);
        }

        Ok(insert)
    }
}

#[cfg(test)]
mod tests {
    use sql_traits::{structs::ParserDB, traits::TableLike};
    use sqlparser::{
        ast::{
            Assignment, AssignmentTarget, Expr, Ident, Insert, ObjectName, ObjectNamePart,
            ObjectNamePartFunction, Statement, TableObject,
        },
        dialect::PostgreSqlDialect,
        parser::Parser,
    };

    use super::resolve_insert_table;
    use crate::{
        errors::Error,
        prelude::{Pg2SqliteOptions, ReverseTranslator},
    };

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

    fn parse_expr(expr: &str) -> Expr {
        Parser::new(&PostgreSqlDialect {})
            .try_with_sql(expr)
            .expect("expr should parse")
            .parse_expr()
            .expect("expr should parse")
    }

    fn schema_from_sql(sql: &str) -> ParserDB {
        let statements = Parser::parse_sql(&PostgreSqlDialect {}, sql).expect("sql should parse");
        ParserDB::from_statements(statements, "test".to_string()).expect("schema should build")
    }

    fn table_object(parts: &[&str]) -> TableObject {
        TableObject::TableName(ObjectName(
            parts
                .iter()
                .map(|part| ObjectNamePart::Identifier(Ident::new(*part)))
                .collect::<Vec<_>>(),
        ))
    }

    #[test]
    fn reverse_translate_insert_translates_assignment_values() {
        let mut insert = parse_insert("INSERT INTO users(id) VALUES (1)");
        insert.assignments = vec![Assignment {
            target: AssignmentTarget::ColumnName(ObjectName(vec![ObjectNamePart::Identifier(
                Ident::new("name"),
            )])),
            value: parse_expr("char(65)"),
        }];

        let schema = empty_schema();
        let options = Pg2SqliteOptions::default();
        let reversed =
            insert.reverse_translate(&schema, &options).expect("insert should reverse-translate");

        assert_eq!(reversed.assignments.len(), 1);
        assert_eq!(reversed.assignments[0].value.to_string(), "chr(65)");
    }

    #[test]
    fn resolve_insert_table_accepts_unqualified_and_public_names() {
        let schema = schema_from_sql("CREATE TABLE users(id INT PRIMARY KEY);");

        let unqualified = resolve_insert_table(&schema, &table_object(&["users"]))
            .expect("unqualified table should resolve");
        assert_eq!(unqualified.table_name(), "users");

        let public_qualified = resolve_insert_table(&schema, &table_object(&["public", "users"]))
            .expect("public-qualified table should resolve");
        assert_eq!(public_qualified.table_name(), "users");
    }

    #[test]
    fn resolve_insert_table_rejects_unknown_non_public_schema_name() {
        let schema = schema_from_sql("CREATE TABLE users(id INT PRIMARY KEY);");

        let err = resolve_insert_table(&schema, &table_object(&["my_custom_app", "users"]))
            .expect_err("non-public schema should be rejected");
        assert!(matches!(err, Error::UnsupportedSchemaQualification { .. }));
        assert!(err.to_string().contains("does not resolve in the translation schema"));
    }

    #[test]
    fn resolve_insert_table_accepts_resolvable_non_public_schema_name() {
        let schema = schema_from_sql(
            "CREATE SCHEMA my_custom_app; CREATE TABLE my_custom_app.users(id INT PRIMARY KEY);",
        );

        let resolved = resolve_insert_table(&schema, &table_object(&["my_custom_app", "users"]))
            .expect("known schema-qualified table should resolve");
        assert_eq!(resolved.table_schema(), Some("my_custom_app"));
        assert_eq!(resolved.table_name(), "users");
    }

    #[test]
    fn resolve_insert_table_rejects_non_identifier_table_segment() {
        let schema = schema_from_sql("CREATE TABLE users(id INT PRIMARY KEY);");
        let table = TableObject::TableName(ObjectName(vec![ObjectNamePart::Function(
            ObjectNamePartFunction { name: Ident::new("remote"), args: vec![] },
        )]));

        let err = resolve_insert_table(&schema, &table)
            .expect_err("function-style segment should be rejected");
        assert!(matches!(err, Error::UnsupportedSchemaQualification { .. }));
        assert!(err.to_string().contains("table segment must be an identifier"));
    }
}
