//! Implementation of the [`ReverseTranslator`] trait for the
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
    errors::LookupError,
    structs::ParserDB,
    traits::{ColumnLike, DatabaseLike, ForeignKeyLike, TableLike, TriggerLike},
};
use sqlparser::ast::{
    Assignment, AssignmentTarget, ConflictTarget, DoUpdate, Expr, Ident, Insert, ObjectName,
    ObjectNamePart, OnConflict, OnConflictAction, OnInsert, ReferentialAction, SqliteOnConflict,
    TableObject,
};

use super::helpers::Reverse;
use crate::{
    errors::Error,
    impls::{
        object_name::resolve_translation_table,
        shared_helpers::{translate_on_conflict_do_update, translate_returning},
        translator_impls::insert::insert_target_scope,
    },
    prelude::ReverseTranslator,
};

/// Refuses `INSERT OR REPLACE` on a table where SQLite's delete-then-insert is
/// observably different from the update PostgreSQL would run.
///
/// SQLite deletes the conflicting rows and inserts a new one, PostgreSQL
/// updates the row in place. Where nothing hangs off the delete the two agree,
/// and those keep translating. Three things hang off it, each measured on both
/// engines: a trigger of any kind fires on one side and not the other, a child
/// row is deleted or blanked on one side and untouched on the other, and a
/// second unique constraint lets SQLite delete two rows where PostgreSQL raises
/// a duplicate key.
fn reject_unfaithful_replace(
    schema: &ParserDB,
    table: &<ParserDB as DatabaseLike>::Table,
) -> Result<(), Error> {
    let name = table.table_name();

    // Any kind, not just a delete trigger: SQLite fires the INSERT one and
    // PostgreSQL fires the UPDATE one, so the two agree only when there is
    // none.
    if let Some(trigger) = table.triggers(schema)?.next() {
        return Err(unfaithful_replace(
            name,
            &format!(
                "the trigger '{}' fires differently: SQLite deletes and inserts, so its INSERT \
                 triggers run, while PostgreSQL updates, so its UPDATE triggers run instead",
                trigger.name()
            ),
        ));
    }

    for other in schema.tables() {
        for foreign_key in other.foreign_keys(schema)? {
            let Some(action) = row_changing_delete_action(foreign_key) else { continue };
            if foreign_key.referenced_table(schema)?.table_name() != name {
                continue;
            }
            return Err(unfaithful_replace(
                name,
                &format!(
                    "'{}' references it ON DELETE {action}, so SQLite's delete reaches those \
                     rows while PostgreSQL's update leaves them alone",
                    other.table_name()
                ),
            ));
        }
    }

    // The primary key counts as one, so a second is a second arbiter.
    if table.unique_indices(schema)?.count() > 1 {
        return Err(unfaithful_replace(
            name,
            "it carries more than one unique constraint, so SQLite resolves a conflict on any of \
             them, deleting every row that collides, while PostgreSQL can name only one arbiter \
             and raises a duplicate key on the others",
        ));
    }

    Ok(())
}

/// The `ON DELETE` actions that change a child row, spelled as written.
fn row_changing_delete_action(
    foreign_key: &<ParserDB as DatabaseLike>::ForeignKey,
) -> Option<&'static str> {
    match foreign_key.attribute().on_delete? {
        ReferentialAction::Cascade => Some("CASCADE"),
        ReferentialAction::SetNull => Some("SET NULL"),
        ReferentialAction::SetDefault => Some("SET DEFAULT"),
        ReferentialAction::NoAction | ReferentialAction::Restrict => None,
    }
}

fn unfaithful_replace(table: &str, because: &str) -> Error {
    Error::reverse_refusal(format!(
        "INSERT OR REPLACE INTO {table} cannot be reversed faithfully, because {because}. \
         SQLite's REPLACE deletes the conflicting rows and inserts a new one, which PostgreSQL \
         has no single statement for. Write the upsert by hand, or a DELETE followed by an \
         INSERT if the delete's effects are what you want."
    ))
}

/// Resolves the target table for reverse upsert reconstruction.
fn resolve_insert_table<'a>(
    schema: &'a ParserDB,
    table: &TableObject,
) -> Result<&'a <ParserDB as DatabaseLike>::Table, Error> {
    let TableObject::TableName(table_name) = table else {
        return Err(Error::reverse_refusal(
            "INSERT OR REPLACE with table function is not supported".to_string(),
        ));
    };

    resolve_translation_table(schema, table_name)?
        .ok_or_else(|| Error::TableNotFoundInSchema { table_name: table_name.to_string() })
}

fn get_primary_key_columns(
    schema: &ParserDB,
    table: &<ParserDB as DatabaseLike>::Table,
) -> Result<Vec<String>, LookupError> {
    Ok(table.primary_key_columns(schema)?.map(|c| c.column_name().to_string()).collect())
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
) -> Result<Vec<Ident>, LookupError> {
    if !explicit_columns.is_empty() {
        return Ok(explicit_columns.to_vec());
    }

    Ok(table.columns(schema)?.map(|column| Ident::new(column.column_name().to_string())).collect())
}

/// Build the `ON CONFLICT DO UPDATE SET` clause for an `INSERT OR REPLACE`.
///
/// `insert_columns` is the explicit column list (or all columns when none was
/// given). `all_table_columns` is every column in schema order: it drives the
/// SET clause so that omitted columns get `DEFAULT` rather than being silently
/// left at their old values. That mirrors SQLite's DELETE-then-INSERT semantics
/// where any column not named in the INSERT reverts to its column default.
///
/// # Errors
///
/// Returns `Error::MissingPrimaryKeyInUpsert` when the table has no primary key
/// or when the insert columns omit a primary key column.
fn build_upsert_on_conflict(
    table_name: &str,
    pk_columns: &[String],
    insert_columns: &[Ident],
    all_table_columns: &[String],
) -> Result<OnInsert, Error> {
    if pk_columns.is_empty() {
        return Err(Error::MissingPrimaryKeyInUpsert {
            table_name: table_name.to_string(),
            pk_columns: Vec::new(),
            insert_columns: insert_columns.iter().map(|c| c.value.clone()).collect(),
        });
    }

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

    let conflict_target =
        ConflictTarget::Columns(pk_columns.iter().map(|c| Ident::new(c.clone())).collect());

    // Iterate all non-PK columns in schema order. Named columns get
    // EXCLUDED.<col> so the incoming value wins. Omitted columns get DEFAULT
    // so they reset to the column default, matching SQLite's DELETE-then-INSERT
    // where the old value is gone and the new row uses defaults for any
    // unspecified column.
    let assignments: Vec<Assignment> = all_table_columns
        .iter()
        .filter(|col| !pk_columns.iter().any(|pk| pk.eq_ignore_ascii_case(col)))
        .map(|col| {
            let col_ident = Ident::new(col.clone());
            let is_named = insert_columns.iter().any(|ic| ic.value.eq_ignore_ascii_case(col));
            let value = if is_named {
                Expr::CompoundIdentifier(vec![Ident::new("EXCLUDED"), col_ident.clone()])
            } else {
                // Column was omitted from the INSERT list: SQLite's replace
                // deletes the old row, so the column gets its default (NULL
                // when there is none). PostgreSQL's DEFAULT keyword resolves
                // the same way inside ON CONFLICT DO UPDATE SET.
                Expr::Identifier(Ident::new("DEFAULT"))
            };
            Assignment {
                target: AssignmentTarget::ColumnName(ObjectName(vec![ObjectNamePart::Identifier(
                    col_ident,
                )])),
                value,
            }
        })
        .collect();

    // PK-only tables have no non-PK columns; DO NOTHING is safe because there
    // is nothing to reset and the row is already present with the right key.
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
    type PostgresEntry = Insert;

    #[allow(clippy::too_many_lines)]
    fn reverse_translate(
        &self,
        schema: &Self::Schema,
        options: &crate::options::TranslationContext<'_>,
    ) -> Result<Self::PostgresEntry, Error> {
        let target_scope = insert_target_scope(self, schema)?;
        let scoped = target_scope.as_ref().map(|scope| options.with_scope(scope));
        let options = scoped.as_ref().unwrap_or(options);
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
        let returning =
            translate_returning::<Reverse>(self.returning.as_ref(), schema, options, &mut |_| {})?;

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
            // PostgreSQL inserts into a table or a view, never into a call, so
            // carrying the call across would emit a statement it refuses to
            // parse. Refused for the shape rather than for the callee, which is
            // what the `OR REPLACE` path already did once it resolved the
            // target.
            TableObject::TableFunction(func) => {
                return Err(Error::reverse_refusal(format!(
                    "INSERT into a table function is not supported: PostgreSQL inserts into a \
                     table or a view, and `{func}` is neither."
                )));
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
                    // INSERT OR REPLACE deletes the conflicting row and inserts
                    // a new one, so every column not named
                    // in the INSERT list reverts to its
                    // default. We must set those columns to DEFAULT in the DO
                    // UPDATE clause; DO NOTHING would
                    // silently preserve the old values.
                    //
                    // The update only stands in for the delete where nothing
                    // observes the delete, which is what the check enforces.
                    let resolved_table = resolve_insert_table(schema, &self.table)?;
                    reject_unfaithful_replace(schema, resolved_table)?;
                    let table_name = resolved_table.table_name().to_string();
                    let pk_columns = get_primary_key_columns(schema, resolved_table)?;
                    let column_idents: Vec<Ident> = self
                        .columns
                        .iter()
                        .filter_map(|c| {
                            c.0.last().and_then(sqlparser::ast::ObjectNamePart::as_ident).cloned()
                        })
                        .collect();
                    let insert_columns =
                        resolve_insert_columns(schema, resolved_table, &column_idents)?;
                    let all_table_columns: Vec<String> = resolved_table
                        .columns(schema)?
                        .map(|c| c.column_name().to_string())
                        .collect();
                    insert.on = Some(build_upsert_on_conflict(
                        &table_name,
                        &pk_columns,
                        &insert_columns,
                        &all_table_columns,
                    )?);
                }
                // INSERT OR FAIL and INSERT OR ABORT abort on conflict, which is
                // exactly what a plain PostgreSQL INSERT does, so the OR clause is
                // dropped and `insert.on` stays None. INSERT OR ROLLBACK also
                // aborts but additionally rolls back any enclosing transaction,
                // a behaviour that cannot be replicated in PostgreSQL from within a
                // single statement, yet the safest translation is still a plain
                // INSERT rather than a silent mutation.
                SqliteOnConflict::Rollback | SqliteOnConflict::Abort | SqliteOnConflict::Fail => {}
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
                // Reverse never unscales a NUMERIC, so there is nothing to
                // move here either.
                &crate::impls::shared_helpers::ColumnRewrites::default(),
                &mut |_| {},
            )?);
        }

        Ok(insert)
    }
}

#[cfg(all(test, feature = "std"))]
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
        let options = crate::options::TranslationContext::from_owned(Pg2SqliteOptions::default());
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
