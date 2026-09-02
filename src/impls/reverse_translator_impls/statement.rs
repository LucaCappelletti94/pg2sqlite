//! Implementation of the [`ReverseTranslator`] trait for the
//! `Statement` type.

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

use sql_traits::{
    structs::ParserDB,
    traits::{DatabaseLike, TableLike},
    utils::identifier_resolution::{identifiers_match, parse_lookup_identifier},
};
use sqlparser::ast::{
    Delete, Expr, Ident, Insert, ObjectName, Query, SetExpr, Statement, Table, TableObject, Update,
    Visit, Visitor,
};
#[cfg(all(test, feature = "std"))]
use sqlparser::ast::{LimitClause, TableFactor};

use super::ident_quoting::normalize_identifier_quotes;
use crate::{
    errors::Error,
    impls::{
        object_name::last_ident,
        placeholder::rewrite_placeholders_for_postgres,
        shared_helpers::debug_variant_name,
        translator_impls::rls::{ensure_usable_rls_table_suffix, resolve_trigger_table_name},
    },
    prelude::{Pg2SqliteOptions, ReverseTranslator},
};

#[derive(Clone, Copy)]
enum Written<'a> {
    Ident(&'a Ident),
    Text(&'a str),
}

impl<'a> Written<'a> {
    fn from_ident(ident: &'a Ident) -> Self {
        Self::Ident(ident)
    }

    fn from_text(text: &'a str) -> Self {
        Self::Text(text)
    }

    fn with_parts<R>(&self, f: impl FnOnce(&str, bool) -> R) -> R {
        match self {
            Self::Ident(ident) => f(&ident.value, ident.quote_style.is_some()),
            Self::Text(text) => {
                let parsed = parse_lookup_identifier(text);
                f(parsed.value(), parsed.is_quoted())
            }
        }
    }

    fn sqlite_matches(&self, other: &Self) -> bool {
        self.with_parts(|left, _| other.with_parts(|right, _| left.eq_ignore_ascii_case(right)))
    }

    fn postgres_matches(&self, other: &Self) -> bool {
        self.with_parts(|left, left_quoted| {
            other.with_parts(|right, right_quoted| {
                identifiers_match(left, left_quoted, right, right_quoted)
            })
        })
    }
}

/// True when `name` ends with `suffix`, ignoring ASCII case.
///
/// Compared as bytes, so a non-ASCII name cannot put the boundary inside a
/// character.
fn ends_with_suffix_ignoring_case(name: &str, suffix: &str) -> bool {
    let (name, suffix) = (name.as_bytes(), suffix.as_bytes());
    name.len() >= suffix.len() && name[name.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
}

/// The names reverse translation must refuse, resolved against the schema.
///
/// Forward translation renames a secured table and leaves a view under the
/// original name, so the physical name it produced is the one with no
/// PostgreSQL counterpart. Asking the schema rather than reading the suffix off
/// the reference keeps the two directions in agreement and stops a table the
/// caller merely named `audit_rls` from being taken for a generated one.
struct RlsTableNames<'a> {
    suffix: &'a str,
    /// Physical names of the secured tables, which is what forward translation
    /// hid behind a view.
    backing: Vec<String>,
    /// Every name the schema declares, in its own spelling.
    declared: Vec<String>,
}

impl<'a> RlsTableNames<'a> {
    fn new(schema: &ParserDB, options: &'a Pg2SqliteOptions) -> Result<Self, Error> {
        ensure_usable_rls_table_suffix(options)?;
        let suffix = options.get_rls_table_suffix();

        let mut backing = Vec::new();
        let mut declared = Vec::new();
        for table in schema.tables() {
            let logical = table.table_name();
            let physical = resolve_trigger_table_name(logical, table, schema, options)?;
            if physical != logical {
                backing.push(physical);
            }
            declared.push(logical.to_string());
        }

        Ok(Self { suffix, backing, declared })
    }

    /// The refusal `reference` earns, if any.
    ///
    /// `written` is the whole name as the statement spelled it, which is what
    /// the message quotes, so a schema-qualified reference reads back the way
    /// the caller wrote it.
    fn refusal(&self, reference: Written<'_>, written: &str) -> Option<Error> {
        reference.with_parts(|value, _| {
            if let Some(backing) = self.backing.iter().find(|name| name.eq_ignore_ascii_case(value))
            {
                return Some(Error::RlsTableDetected {
                    table_name: backing.clone(),
                    suffix: self.suffix.to_string(),
                });
            }

            if self.declared.iter().any(|name| name.eq_ignore_ascii_case(value)) {
                return None;
            }

            ends_with_suffix_ignoring_case(value, self.suffix).then(|| {
                Error::RlsTableDetected {
                    table_name: written.to_string(),
                    suffix: self.suffix.to_string(),
                }
            })
        })
    }
}

/// Refuses a reference SQLite reads as a CTE and PostgreSQL reads as a table.
fn cte_shadow_disagreement(alias: &str, written: &str) -> Error {
    Error::reverse_refusal(format!(
        "WITH \"{alias}\" hides the row-security backing table {written} in SQLite, which \
         matches a name without regard to case, while PostgreSQL keeps the capitals of a \
         quoted name and would read {written} as the table instead. Spell the CTE name the \
         way the reference spells it."
    ))
}

/// What the CTE names in scope say about a relation reference.
enum CteBinding {
    /// No CTE in scope answers the reference.
    Absent,
    /// Both databases bind the reference to the CTE, so it names no table.
    Shadowed,
    /// SQLite binds the reference to the CTE while PostgreSQL does not, because
    /// the alias is delimited and spelled differently.
    SqliteOnly(String),
}

fn check_table_for_rls(name: &ObjectName, names: &RlsTableNames<'_>) -> Result<(), Error> {
    let Some(last) = last_ident(name) else { return Ok(()) };
    match names.refusal(Written::from_ident(last), &name.to_string()) {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn check_table_object_for_rls(table: &TableObject, names: &RlsTableNames<'_>) -> Result<(), Error> {
    match table {
        TableObject::TableName(name) => check_table_for_rls(name, names),
        TableObject::TableFunction(_) | TableObject::TableQuery(_) => Ok(()),
    }
}

/// The name a `TABLE t` set expression reads, spelled as the statement wrote
/// it.
fn table_command_written_name(table: &Table) -> Option<String> {
    let table_name = table.table_name.as_ref()?;
    Some(
        table
            .schema_name
            .as_ref()
            .map_or_else(|| table_name.clone(), |schema| format!("{schema}.{table_name}")),
    )
}

/// One query's CTE names and their visibility while its children are visited.
struct CteScope {
    aliases: Vec<Ident>,
    body_visibility: Vec<(usize, usize)>,
    visible_aliases: usize,
    restore_parent_visibility: Option<usize>,
}

/// Walks a node refusing any reference that reaches a row-security backing
/// table.
struct RlsAstVisitor<'a> {
    names: &'a RlsTableNames<'a>,
    cte_scopes: Vec<CteScope>,
}

impl RlsAstVisitor<'_> {
    fn cte_binding(&self, reference: Written<'_>) -> CteBinding {
        for scope in self.cte_scopes.iter().rev() {
            for alias in scope.aliases.iter().take(scope.visible_aliases) {
                let alias = Written::from_ident(alias);
                if !alias.sqlite_matches(&reference) {
                    continue;
                }
                return if alias.postgres_matches(&reference) {
                    CteBinding::Shadowed
                } else {
                    CteBinding::SqliteOnly(alias.with_parts(|value, _| value.to_string()))
                };
            }
        }
        CteBinding::Absent
    }

    /// The refusal a relation reference earns, with the CTE names in scope
    /// taken into account.
    fn relation_refusal(&self, reference: Written<'_>, written: &str) -> Option<Error> {
        match self.cte_binding(reference) {
            CteBinding::Shadowed => None,
            CteBinding::Absent => self.names.refusal(reference, written),
            // Only a name the guard would otherwise refuse is worth refusing
            // here. Whether a delimited mixed-case identifier reaches the same
            // object in both databases is a question this crate leaves alone,
            // see `super::ident_quoting`.
            CteBinding::SqliteOnly(alias) => {
                self.names
                    .refusal(reference, written)
                    .map(|_| cte_shadow_disagreement(&alias, written))
            }
        }
    }

    fn pop_query_scope(&mut self) {
        let restore = self.cte_scopes.pop().and_then(|scope| scope.restore_parent_visibility);
        if let Some(restore) = restore
            && let Some(parent) = self.cte_scopes.last_mut()
        {
            parent.visible_aliases = restore;
        }
    }

    fn check_set_expr(&self, set_expr: &SetExpr) -> Result<(), Error> {
        match set_expr {
            SetExpr::SetOperation { left, right, .. } => {
                self.check_set_expr(left)?;
                self.check_set_expr(right)
            }
            SetExpr::Table(table) => {
                let Some(written) = table_command_written_name(table) else { return Ok(()) };
                let name = table.table_name.as_deref().unwrap_or_default();
                match self.relation_refusal(Written::from_text(name), &written) {
                    Some(error) => Err(error),
                    None => Ok(()),
                }
            }
            SetExpr::Merge(_) => {
                Err(Error::reverse_refusal(
                    "MERGE set expressions are not supported in reverse translation".to_string(),
                ))
            }
            SetExpr::Query(_)
            | SetExpr::Select(_)
            | SetExpr::Insert(_)
            | SetExpr::Update(_)
            | SetExpr::Delete(_)
            | SetExpr::Values(_) => Ok(()),
        }
    }
}

impl Visitor for RlsAstVisitor<'_> {
    type Break = Box<Error>;

    fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<Self::Break> {
        let address = core::ptr::from_ref(query) as usize;
        let restore_parent_visibility = self.cte_scopes.last_mut().and_then(|scope| {
            let child_visibility = scope
                .body_visibility
                .iter()
                .find_map(|(child, visible)| (*child == address).then_some(*visible));
            child_visibility.map(|visible| {
                let restore = scope.visible_aliases;
                scope.visible_aliases = visible;
                restore
            })
        });
        let (aliases, body_visibility) = query.with.as_ref().map_or_else(
            || (Vec::new(), Vec::new()),
            |with| {
                let recursive = usize::from(with.recursive);
                let aliases =
                    with.cte_tables.iter().map(|cte| cte.alias.name.clone()).collect::<Vec<_>>();
                let body_visibility = with
                    .cte_tables
                    .iter()
                    .enumerate()
                    .map(|(index, cte)| {
                        (core::ptr::from_ref(cte.query.as_ref()) as usize, index + recursive)
                    })
                    .collect();
                (aliases, body_visibility)
            },
        );
        let visible_aliases = aliases.len();
        self.cte_scopes.push(CteScope {
            aliases,
            body_visibility,
            visible_aliases,
            restore_parent_visibility,
        });

        match self.check_set_expr(query.body.as_ref()) {
            Ok(()) => ControlFlow::Continue(()),
            Err(err) => {
                self.pop_query_scope();
                ControlFlow::Break(Box::new(err))
            }
        }
    }

    fn post_visit_query(&mut self, _query: &Query) -> ControlFlow<Self::Break> {
        self.pop_query_scope();
        ControlFlow::Continue(())
    }

    fn pre_visit_relation(&mut self, relation: &ObjectName) -> ControlFlow<Self::Break> {
        let Some(last) = last_ident(relation) else { return ControlFlow::Continue(()) };
        match self.relation_refusal(Written::from_ident(last), &relation.to_string()) {
            None => ControlFlow::Continue(()),
            Some(err) => ControlFlow::Break(Box::new(err)),
        }
    }

    fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<Self::Break> {
        if let Expr::MatchAgainst { .. } = expr {
            return ControlFlow::Break(Box::new(Error::UnsupportedRlsExpressionVariant {
                expr_variant: debug_variant_name(expr),
                expression: expr.to_string(),
            }));
        }
        ControlFlow::Continue(())
    }
}

fn walk_for_rls<T: Visit>(node: &T, names: &RlsTableNames<'_>) -> Result<(), Error> {
    let mut visitor = RlsAstVisitor { names, cte_scopes: Vec::new() };
    match node.visit(&mut visitor) {
        ControlFlow::Continue(()) => Ok(()),
        ControlFlow::Break(err) => Err(*err),
    }
}

fn run_rls_visitor<T: Visit>(
    node: &T,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<(), Error> {
    walk_for_rls(node, &RlsTableNames::new(schema, options)?)
}

#[cfg(all(test, feature = "std"))]
fn check_table_factor_for_rls(
    factor: &TableFactor,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<(), Error> {
    run_rls_visitor(factor, schema, options)
}

/// Check an expression tree for RLS table references in subqueries.
#[cfg(all(test, feature = "std"))]
fn check_expr_for_rls(
    expr: &Expr,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<(), Error> {
    run_rls_visitor(expr, schema, options)
}

#[cfg(all(test, feature = "std"))]
fn check_set_expr_for_rls(
    set_expr: &SetExpr,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<(), Error> {
    let names = RlsTableNames::new(schema, options)?;
    RlsAstVisitor { names: &names, cte_scopes: Vec::new() }.check_set_expr(set_expr)?;
    walk_for_rls(set_expr, &names)
}

#[cfg(all(test, feature = "std"))]
fn check_limit_clause_for_rls(
    limit_clause: &LimitClause,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<(), Error> {
    run_rls_visitor(limit_clause, schema, options)
}

fn check_query_for_rls(
    query: &Query,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<(), Error> {
    run_rls_visitor(query, schema, options)
}

fn check_insert_for_rls(
    insert: &Insert,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<(), Error> {
    let names = RlsTableNames::new(schema, options)?;
    check_table_object_for_rls(&insert.table, &names)?;
    walk_for_rls(insert, &names)
}

fn check_update_for_rls(
    update: &Update,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<(), Error> {
    run_rls_visitor(update, schema, options)
}

fn check_delete_for_rls(
    delete: &Delete,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<(), Error> {
    run_rls_visitor(delete, schema, options)
}

impl ReverseTranslator for Statement {
    type Schema = ParserDB;
    type PostgresEntry = Statement;

    fn reverse_translate(
        &self,
        schema: &Self::Schema,
        options: &crate::options::TranslationContext<'_>,
    ) -> Result<Self::PostgresEntry, Error> {
        ensure_usable_rls_table_suffix(options)?;
        let mut translated = match self {
            Statement::Insert(insert) => {
                check_insert_for_rls(insert, schema, options)?;

                Statement::Insert(insert.reverse_translate(schema, options)?)
            }
            Statement::Update(update) => {
                check_update_for_rls(update, schema, options)?;

                Statement::Update(update.reverse_translate(schema, options)?)
            }
            Statement::Delete(delete) => {
                check_delete_for_rls(delete, schema, options)?;

                Statement::Delete(delete.reverse_translate(schema, options)?)
            }
            Statement::Query(query) => {
                check_query_for_rls(query, schema, options)?;

                Statement::Query(Box::new(query.reverse_translate(schema, options)?))
            }
            // Transaction control statements pass through unchanged
            Statement::Commit { .. }
            | Statement::Rollback { .. }
            | Statement::StartTransaction { .. }
            | Statement::Savepoint { .. }
            | Statement::ReleaseSavepoint { .. } => self.clone(),
            // Non-DML statements are not supported for reverse translation
            other => {
                let variant_name = debug_variant_name(other);
                return Err(Error::UnsupportedReverseStatement { statement_type: variant_name });
            }
        };

        // PostgreSQL accepts only numbered `$N` bind parameters, so map the
        // SQLite placeholder tokens the parse produced. Runs before identifier
        // normalization; a named placeholder aborts with a typed error.
        rewrite_placeholders_for_postgres(&mut translated)?;

        // Reverse output is presented as PostgreSQL, which accepts only
        // double-quoted identifiers. Rewrite any backtick or bracket quoting
        // the SQLite parse produced.
        normalize_identifier_quotes(&mut translated);
        Ok(translated)
    }
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use sql_traits::structs::ParserDB;
    use sqlparser::{
        ast::{
            AccessExpr, Expr, LimitClause, Offset, Query, SetExpr, Statement, Subscript,
            TableFactor,
        },
        dialect::PostgreSqlDialect,
        parser::Parser,
    };

    use super::{
        check_expr_for_rls, check_limit_clause_for_rls, check_query_for_rls,
        check_set_expr_for_rls, check_table_factor_for_rls,
    };
    use crate::{
        errors::Error,
        prelude::{Pg2SqliteOptions, ReverseTranslator},
    };

    fn empty_schema() -> ParserDB {
        ParserDB::from_statements(Vec::new(), "test".to_string()).unwrap()
    }

    fn parse_expr(expr: &str) -> Expr {
        Parser::new(&PostgreSqlDialect {}).try_with_sql(expr).unwrap().parse_expr().unwrap()
    }

    fn parse_query(sql: &str) -> Query {
        let stmt = Parser::parse_sql(&PostgreSqlDialect {}, sql).unwrap().remove(0);
        match stmt {
            Statement::Query(query) => *query,
            other => panic!("expected query, got: {other:?}"),
        }
    }

    #[test]
    fn check_expr_for_rls_accepts_many_expression_variants() {
        let options = crate::options::TranslationContext::from_owned(Pg2SqliteOptions::default());
        let expressions = vec![
            "a = ANY(b)",
            "a = ALL(b)",
            "CASE WHEN x > 0 THEN y ELSE z END",
            "TRIM(BOTH 'x' FROM col)",
            "SUBSTRING(col FROM 1 FOR 2)",
            "OVERLAY(col PLACING 'x' FROM 1 FOR 1)",
            "POSITION('x' IN col)",
            "a AT TIME ZONE 'UTC'",
            "a LIKE b",
            "a ILIKE b",
            "a SIMILAR TO b",
            "a RLIKE b",
            "ARRAY[1,2]",
            "(SELECT 1)",
            "EXISTS (SELECT 1)",
            "x IN (SELECT 1)",
            "(1, 2)",
            "INTERVAL '1 day'",
            "'abc' COLLATE \"C\"",
            "foo[0]",
        ];

        for raw in expressions {
            let expr = parse_expr(raw);
            check_expr_for_rls(&expr, &empty_schema(), &options).unwrap();
        }
    }

    #[test]
    fn check_expr_for_rls_rejects_subquery_in_subscript_index() {
        let options = crate::options::TranslationContext::from_owned(Pg2SqliteOptions::default());
        let expr = Expr::CompoundFieldAccess {
            root: Box::new(parse_expr("payload")),
            access_chain: vec![AccessExpr::Subscript(Subscript::Index {
                index: Expr::Subquery(Box::new(parse_query("SELECT id FROM users_rls LIMIT 1"))),
            })],
        };

        let err = check_expr_for_rls(&expr, &empty_schema(), &options).unwrap_err();
        assert!(err.to_string().contains("users_rls"));
    }

    #[test]
    fn check_query_for_rls_covers_with_order_by_limit_fetch_and_function_shapes() {
        let options = crate::options::TranslationContext::from_owned(Pg2SqliteOptions::default());
        let query = parse_query(
            r#"
            WITH c AS (SELECT 1 AS id)
            SELECT
                id,
                percentile_disc(0.5) WITHIN GROUP (ORDER BY id),
                sum(id) FILTER (WHERE id > 0) OVER (PARTITION BY id ORDER BY id)
            FROM c
            WHERE id IN (SELECT id FROM c)
            GROUP BY id
            HAVING id > 0
            ORDER BY id
            LIMIT 10 OFFSET 1
            FETCH FIRST 5 ROWS ONLY
            "#,
        );

        check_query_for_rls(&query, &empty_schema(), &options).unwrap();
    }

    #[test]
    fn check_set_expr_for_rls_handles_insert_update_delete_values_and_table_variants() {
        let options = crate::options::TranslationContext::from_owned(Pg2SqliteOptions::default());

        let insert_stmt =
            Parser::parse_sql(&PostgreSqlDialect {}, "INSERT INTO users(id) VALUES (1)")
                .unwrap()
                .remove(0);
        if let Statement::Insert(insert) = insert_stmt {
            check_set_expr_for_rls(
                &SetExpr::Insert(Statement::Insert(insert)),
                &empty_schema(),
                &options,
            )
            .unwrap();
        } else {
            panic!("expected insert");
        }

        let update_stmt =
            Parser::parse_sql(&PostgreSqlDialect {}, "UPDATE users SET id = 1").unwrap().remove(0);
        if let Statement::Update(update) = update_stmt {
            check_set_expr_for_rls(
                &SetExpr::Update(Statement::Update(update)),
                &empty_schema(),
                &options,
            )
            .unwrap();
        } else {
            panic!("expected update");
        }

        let delete_stmt =
            Parser::parse_sql(&PostgreSqlDialect {}, "DELETE FROM users WHERE id = 1")
                .unwrap()
                .remove(0);
        if let Statement::Delete(delete) = delete_stmt {
            check_set_expr_for_rls(
                &SetExpr::Delete(Statement::Delete(delete)),
                &empty_schema(),
                &options,
            )
            .unwrap();
        } else {
            panic!("expected delete");
        }

        let values_query = parse_query("VALUES (1), (2)");
        check_set_expr_for_rls(values_query.body.as_ref(), &empty_schema(), &options).unwrap();

        let table_expr = SetExpr::Table(Box::new(sqlparser::ast::Table {
            table_name: Some("users".to_string()),
            schema_name: None,
        }));
        check_set_expr_for_rls(&table_expr, &empty_schema(), &options).unwrap();

        let rls_table_expr = SetExpr::Table(Box::new(sqlparser::ast::Table {
            table_name: Some("users_rls".to_string()),
            schema_name: None,
        }));
        let err = check_set_expr_for_rls(&rls_table_expr, &empty_schema(), &options).unwrap_err();
        assert!(err.to_string().contains("users_rls"));

        let merge_stmt = Parser::parse_sql(&PostgreSqlDialect {}, "COMMIT;").unwrap().remove(0);
        let merge_expr = SetExpr::Merge(merge_stmt);
        let err = check_set_expr_for_rls(&merge_expr, &empty_schema(), &options)
            .expect_err("merge set expr should be rejected");
        assert!(err.to_string().contains("MERGE set expressions"));
    }

    #[test]
    fn check_limit_clause_for_rls_handles_offset_comma_limit_variant() {
        let options = crate::options::TranslationContext::from_owned(Pg2SqliteOptions::default());
        let offset_comma =
            LimitClause::OffsetCommaLimit { offset: parse_expr("1"), limit: parse_expr("10") };
        check_limit_clause_for_rls(&offset_comma, &empty_schema(), &options).unwrap();

        let limit_offset = LimitClause::LimitOffset {
            limit: Some(parse_expr("10")),
            offset: Some(Offset { value: parse_expr("1"), rows: sqlparser::ast::OffsetRows::None }),
            limit_by: vec![parse_expr("2")],
        };
        check_limit_clause_for_rls(&limit_offset, &empty_schema(), &options).unwrap();
    }

    #[test]
    fn reverse_translate_rejects_rls_backing_tables_and_non_dml_statements() {
        let schema = empty_schema();
        let options = crate::options::TranslationContext::from_owned(Pg2SqliteOptions::default());

        let query_stmt =
            Parser::parse_sql(&PostgreSqlDialect {}, "SELECT * FROM users_rls").unwrap().remove(0);
        let err = query_stmt.reverse_translate(&schema, &options).unwrap_err();
        assert!(err.to_string().contains("Direct access to RLS backing table"));

        let non_dml = Parser::parse_sql(&PostgreSqlDialect {}, "VACUUM").unwrap().remove(0);
        let err = non_dml.reverse_translate(&schema, &options).unwrap_err();
        assert!(err.to_string().contains("Reverse translation only supports DML statements"));
    }

    #[test]
    fn check_table_factor_and_set_expr_cover_query_fallback_paths() {
        let options = crate::options::TranslationContext::from_owned(Pg2SqliteOptions::default());

        let table_fn_query = parse_query("SELECT * FROM generate_series(1, 2)");
        let sqlparser::ast::SetExpr::Select(select) = table_fn_query.body.as_ref() else {
            panic!("expected select");
        };
        check_table_factor_for_rls(&select.from[0].relation, &empty_schema(), &options).unwrap();

        let manual_table_function =
            TableFactor::TableFunction { expr: parse_expr("generate_series(1, 2)"), alias: None };
        check_table_factor_for_rls(&manual_table_function, &empty_schema(), &options).unwrap();

        let set_expr = SetExpr::Query(Box::new(parse_query("SELECT 1")));
        check_set_expr_for_rls(&set_expr, &empty_schema(), &options).unwrap();
    }

    #[test]
    fn check_expr_for_rls_rejects_grouping_sets_with_rls_subquery() {
        let options = crate::options::TranslationContext::from_owned(Pg2SqliteOptions::default());
        let expr = Expr::GroupingSets(vec![vec![Expr::Subquery(Box::new(parse_query(
            "SELECT id FROM users_rls LIMIT 1",
        )))]]);

        let err = check_expr_for_rls(&expr, &empty_schema(), &options)
            .expect_err("GROUPING SETS expressions with RLS-backed subqueries should be rejected");
        assert!(err.to_string().contains("users_rls"));
    }

    #[test]
    fn check_query_for_rls_rejects_select_side_paths_with_rls_subqueries() {
        let options = crate::options::TranslationContext::from_owned(Pg2SqliteOptions::default());
        let mut query = parse_query("SELECT id FROM users");
        let SetExpr::Select(select) = query.body.as_mut() else {
            panic!("expected select");
        };

        select.prewhere =
            Some(Expr::Subquery(Box::new(parse_query("SELECT id FROM users_rls LIMIT 1"))));
        select.cluster_by =
            vec![Expr::Subquery(Box::new(parse_query("SELECT id FROM users_rls LIMIT 1")))];
        select.distribute_by =
            vec![Expr::Subquery(Box::new(parse_query("SELECT id FROM users_rls LIMIT 1")))];
        select.sort_by = vec![sqlparser::ast::OrderByExpr {
            expr: Expr::Subquery(Box::new(parse_query("SELECT id FROM users_rls LIMIT 1"))),
            options: sqlparser::ast::OrderByOptions { sort: None, nulls_first: None },
            with_fill: None,
        }];

        let err = check_query_for_rls(&query, &empty_schema(), &options).expect_err(
            "Select-side expression vectors should be traversed for RLS-backed subqueries",
        );
        assert!(err.to_string().contains("users_rls"));
    }

    #[test]
    fn check_query_for_rls_rejects_query_settings_and_pipe_exprs() {
        let options = crate::options::TranslationContext::from_owned(Pg2SqliteOptions::default());
        let mut query = parse_query("SELECT id FROM users");
        query.settings = Some(vec![sqlparser::ast::Setting {
            key: sqlparser::ast::Ident::new("x"),
            value: Expr::Subquery(Box::new(parse_query("SELECT id FROM users_rls LIMIT 1"))),
        }]);
        query.pipe_operators = vec![sqlparser::ast::PipeOperator::Where {
            expr: Expr::Subquery(Box::new(parse_query("SELECT id FROM users_rls LIMIT 1"))),
        }];

        let err = check_query_for_rls(&query, &empty_schema(), &options).expect_err(
            "Query settings and pipe operators should be traversed for RLS-backed subqueries",
        );
        assert!(err.to_string().contains("users_rls"));
    }

    #[test]
    fn check_expr_for_rls_rejects_function_subquery_arguments() {
        let options = crate::options::TranslationContext::from_owned(Pg2SqliteOptions::default());
        let expr = Expr::Function(sqlparser::ast::Function {
            name: sqlparser::ast::ObjectName::from(vec![sqlparser::ast::Ident::new("array")]),
            uses_odbc_syntax: false,
            parameters: sqlparser::ast::FunctionArguments::None,
            args: sqlparser::ast::FunctionArguments::Subquery(Box::new(parse_query(
                "SELECT id FROM users_rls LIMIT 1",
            ))),
            filter: None,
            null_treatment: None,
            over: None,
            within_group: Vec::new(),
        });

        let err = check_expr_for_rls(&expr, &empty_schema(), &options)
            .expect_err("Function subquery arguments should be checked for RLS-backed tables");
        assert!(err.to_string().contains("users_rls"));
    }

    #[test]
    fn check_expr_for_rls_rejects_unhandled_match_against_variant() {
        let options = crate::options::TranslationContext::from_owned(Pg2SqliteOptions::default());
        let expr = Expr::MatchAgainst {
            columns: vec![sqlparser::ast::ObjectName::from(vec![sqlparser::ast::Ident::new(
                "body",
            )])],
            match_value: sqlparser::ast::Value::SingleQuotedString("term".to_string()).into(),
            opt_search_modifier: None,
        };

        let err = check_expr_for_rls(&expr, &empty_schema(), &options)
            .expect_err("unhandled expression variants must fail closed in RLS checks");
        assert!(matches!(err, Error::UnsupportedRlsExpressionVariant { .. }));
        assert!(err.to_string().contains("MatchAgainst"));
    }
}
