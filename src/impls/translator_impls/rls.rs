//! RLS (Row-Level Security) translation from PostgreSQL to SQLite.
//!
//! This module handles the translation of PostgreSQL RLS policies to SQLite
//! by generating:
//! 1. A renamed inner table (e.g., `documents_rls`) containing the actual data
//! 2. A view with the original table name that filters rows based on policies
//! 3. INSTEAD OF triggers on the view for INSERT, UPDATE, DELETE operations
//!
//! The triggers are the only writers left once a table is split, so they carry
//! what a view cannot: a column's `DEFAULT`, which the INSERT trigger applies
//! itself, and a computed column, which it refuses to be told. A view gives
//! SQLite no way to tell an omitted column from one written NULL, so a default
//! answers for both.

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
use core::{fmt::Write, ops::ControlFlow};

use sql_traits::{
    errors::LookupError,
    structs::ParserDB,
    traits::{ColumnLike, DatabaseLike, PolicyLike, TableLike},
};
use sqlparser::ast::{
    ColumnOption, CreatePolicy, CreatePolicyCommand, CreatePolicyType, CreateTable, Expr, Function,
    FunctionArg, FunctionArgExpr, FunctionArgumentClause, FunctionArgumentList, FunctionArguments,
    HavingBound, Ident, JoinConstraint, JoinOperator, ListAggOnOverflow, Owner, Statement,
    TableFactor, Value, VisitMut, VisitorMut, WindowType,
};

use crate::{
    errors::Error,
    impls::{
        expr_helpers::{for_each_child_expr, map_expr_children},
        function_helpers::simple_function_expr,
        generated_sql::{parse_generated_sql, parse_single_generated_sql},
        object_name::{
            append_suffix, last_ident, prefixed_quoted_identifier, quote_identifier,
            sql_string_literal,
        },
        session_variable,
        shared_helpers::{join_constraint_mut, join_constraint_ref},
        translator_impls::column::declared_default,
    },
    options::Pg2SqliteOptions,
    traits::{SessionVariablePattern, TranslationOptions, translator::Translator},
};

/// A column of a guarded table, as the write triggers have to see it.
///
/// A view is not a table: it carries no defaults, and it cannot compute a
/// generated column. Both facts have to reach the triggers, which are the only
/// writers left once the table is split.
struct TriggerColumn {
    /// The column's name as the table declares it.
    name: String,
    /// The `DEFAULT` the backing table declares, untranslated. `None` when the
    /// column declares none.
    default: Option<Expr>,
    /// The expression SQLite computes the column from, untranslated, for a
    /// generated column. Such a column refuses to be written and the view
    /// cannot compute it, so a guard reading it has to compute it instead.
    generated: Option<Expr>,
}

impl TriggerColumn {
    /// The value the forwarding write hands the backing table.
    ///
    /// A column the caller omitted arrives NULL, indistinguishable from a NULL
    /// the caller wrote, so a declared default answers for both.
    fn forwarded_value(&self) -> Expr {
        let new_value = prefixed_column_expr("NEW", &self.name);
        match &self.default {
            Some(default) => coalesce(new_value, default.clone()),
            None => new_value,
        }
    }

    const fn is_generated(&self) -> bool {
        self.generated.is_some()
    }
}

/// Reads every column of the guarded table the way the write triggers need it.
///
/// # Errors
///
/// Propagates a lookup failure, and a default that cannot be expressed at the
/// column's scale, or a UUID BLOB column whose default is not a valid UUID.
fn trigger_columns(
    table: &CreateTable,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Vec<TriggerColumn>, Error> {
    table
        .columns(schema)?
        .map(|column| {
            let attribute = column.attribute();
            let generated = attribute.options.iter().find_map(|option| {
                match &option.option {
                    ColumnOption::Generated { generation_expr: Some(expr), .. } => {
                        Some(expr.clone())
                    }
                    _ => None,
                }
            });
            Ok(TriggerColumn {
                name: column.column_name().to_owned(),
                default: declared_default(attribute, options)?,
                generated,
            })
        })
        .collect()
}

/// `NEW."column"` and friends, as an expression rather than rendered text,
/// quoted exactly where [`quote_identifier`] quotes so the emitted trigger
/// reads the way the rest of it does.
fn prefixed_column_expr(prefix: &str, column: &str) -> Expr {
    let ident = if quote_identifier(column).starts_with('"') {
        Ident::with_quote('"', column)
    } else {
        Ident::new(column)
    };
    Expr::CompoundIdentifier(vec![Ident::new(prefix), ident])
}

fn coalesce(value: Expr, fallback: Expr) -> Expr {
    simple_function_expr("COALESCE", vec![value, fallback], None)
}

/// What a `NEW.<column>` reference in a write guard stands for, when the value
/// reaching the trigger is not the value the row will carry.
type GuardSubstitution = (String, Expr);

/// Rewrites those references, so a guard judges the row as it will be stored.
struct GuardFolder<'a> {
    substitutions: &'a [GuardSubstitution],
}

impl VisitorMut for GuardFolder<'_> {
    type Break = ();

    /// Folded on the way out, so a `NEW.<column>` the fold itself writes is not
    /// folded again.
    fn post_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<Self::Break> {
        if let Expr::CompoundIdentifier(parts) = expr
            && let [prefix, column] = parts.as_slice()
            && prefix.value.eq_ignore_ascii_case("NEW")
            && let Some((_, replacement)) =
                self.substitutions.iter().find(|(name, _)| name.eq_ignore_ascii_case(&column.value))
        {
            *expr = replacement.clone();
        }
        ControlFlow::Continue(())
    }
}

fn fold_guard(expr: &mut Expr, substitutions: &[GuardSubstitution]) {
    let _: ControlFlow<()> = VisitMut::visit(expr, &mut GuardFolder { substitutions });
}

/// Which write a guard sits in, which decides what a `NEW.<column>` reference
/// is missing.
#[derive(Clone, Copy, Eq, PartialEq)]
enum GuardKind {
    /// A column the caller left out arrives NULL, so a default answers for it.
    /// A generated column arrives NULL too, and SQLite will compute it.
    Insert,
    /// SQLite fills `NEW` from the row on disk, so only a generated column is
    /// missing: the view holds the value computed before the update.
    Update,
}

/// What each `NEW.<column>` reference in a guard has to be replaced by.
///
/// # Errors
///
/// Propagates a schema lookup failure.
fn guard_substitutions(
    columns: &[TriggerColumn],
    kind: GuardKind,
    options: &Pg2SqliteOptions,
    table: &CreateTable,
    schema: &ParserDB,
) -> Result<Vec<GuardSubstitution>, Error> {
    let defaults: Vec<GuardSubstitution> = if kind == GuardKind::Insert {
        columns
            .iter()
            .filter(|column| column.default.is_some())
            .map(|column| (column.name.clone(), column.forwarded_value()))
            .collect()
    } else {
        Vec::new()
    };

    let ctx = RlsTriggerContext::new(table, options);
    let lowercased_columns: Vec<String> =
        table.columns(schema)?.map(|c| c.column_name().to_lowercase()).collect();
    let facts = ResolvedSchemaFacts { lowercased_columns: &lowercased_columns };

    let mut substitutions = defaults.clone();
    for column in columns.iter().filter(|column| column.is_generated()) {
        let Some(expression) = column.generated.as_ref() else { continue };
        // The generation expression names bare columns of this same row, so it
        // goes through the ordinary transformer to reach them as `NEW`. Folding
        // the defaults into it afterwards covers a column computed from one the
        // caller left out. PostgreSQL forbids a generation expression from
        // reading another generated column, so one pass is enough.
        let mut computed = transform_expr(
            expression,
            options,
            table,
            schema,
            Some("NEW"),
            Some(ctx.as_rename_tuple()),
            facts,
        );
        fold_guard(&mut computed, &defaults);
        substitutions.push((column.name.clone(), Expr::Nested(Box::new(computed))));
    }

    Ok(substitutions)
}

/// Refuses a write that supplies a value for a computed column, the way
/// PostgreSQL refuses one.
///
/// SQLite computes such a column and will not be told its value, so the
/// forwarding write leaves it out entirely. Leaving it out is invisible to the
/// caller, who would see a supplied value vanish, hence the guard. What the
/// guard cannot catch is a caller who writes NULL: on the insert path that is
/// indistinguishable from omitting the column, and on the update path from
/// leaving it alone.
fn refuse_computed_writes(columns: &[TriggerColumn], table_name: &str, guard: GuardKind) -> String {
    columns.iter().filter(|column| column.is_generated()).fold(String::new(), |mut guards, column| {
        let supplied = match guard {
            GuardKind::Insert => {
                format!("{} IS NOT NULL", prefixed_quoted_identifier("NEW", &column.name))
            }
            // SQLite fills NEW from the row on disk, so a difference from OLD
            // is the statement having assigned the column.
            GuardKind::Update => {
                format!(
                    "{} IS DISTINCT FROM {}",
                    prefixed_quoted_identifier("NEW", &column.name),
                    prefixed_quoted_identifier("OLD", &column.name)
                )
            }
        };
        let message = sql_string_literal(&format!(
            "cannot write to generated column \"{}\" of \"{table_name}\": SQLite computes it from \
             the other columns",
            column.name
        ));
        let _ = write!(guards, "SELECT RAISE(ABORT, {message}) WHERE {supplied};\n    ");
        guards
    })
}

const RLS_VIOLATION_ERROR: &str = "new row violates row-level security policy";

/// True when `expr` is the boolean literal `true`, possibly wrapped in
/// redundant parentheses. Such a predicate constrains nothing.
fn is_true_literal(expr: &Expr) -> bool {
    match expr {
        Expr::Value(value) => matches!(value.value, Value::Boolean(true)),
        Expr::Nested(inner) => is_true_literal(inner),
        _ => false,
    }
}

/// Which clause of a policy supplies the predicate.
#[derive(Clone, Copy)]
enum PolicyClause {
    /// `USING`, which selects the existing rows a command may touch.
    Using,
    /// `WITH CHECK`, which constrains the row a command writes.
    Check,
}

/// How a policy set constrains rows once permissive and restrictive policies
/// are combined.
enum PolicyPredicate {
    /// No permissive policy grants access, so no row qualifies.
    DenyAll,
    /// Nothing narrows the row set, so every row qualifies.
    AllowAll,
    /// Rows satisfying this SQL boolean expression qualify.
    Expr(String),
}

/// Combines a policy set the way PostgreSQL does:
/// `(PERMISSIVE_1 OR PERMISSIVE_2 OR ...) AND RESTRICTIVE_1 AND RESTRICTIVE_2`.
///
/// Permissive policies OR together to grant access. Restrictive policies AND on
/// top to remove it. Two consequences the caller must honour:
///
/// * A set with no permissive policy denies every row, because nothing granted
///   access. That covers both an empty set and a restrictive-only set.
/// * A permissive policy carrying no predicate grants every row, so it
///   contributes TRUE rather than nothing. Same for a restrictive one.
///
/// `substitutions` says what a `NEW.<column>` reference stands for where the
/// value reaching the trigger is not the value the row will carry: a default
/// for a column the caller left out, and the computation for a generated one.
/// Empty for a predicate resolved against `OLD` or against the table itself.
fn combine_policy_predicates(
    policies: &[&CreatePolicy],
    clause: PolicyClause,
    prefix: Option<&str>,
    options: &Pg2SqliteOptions,
    table: &CreateTable,
    schema: &ParserDB,
    substitutions: &[GuardSubstitution],
) -> Result<PolicyPredicate, Error> {
    // The backing table's name is what a policy naming its own table has to
    // resolve to, and it follows from the table and the options, so it is read
    // here rather than passed in and risked disagreeing with the trigger.
    let ctx = RlsTriggerContext::new(table, options);
    let table_rename = Some(ctx.as_rename_tuple());

    // Both sets are resolved once here rather than inside the recursive
    // transformer, which keeps that transformer total and avoids re-querying the
    // schema at every node it visits.
    let lowercased_columns: Vec<String> =
        table.columns(schema)?.map(|c| c.column_name().to_lowercase()).collect();
    let facts = ResolvedSchemaFacts { lowercased_columns: &lowercased_columns };

    let mut permissive = Vec::new();
    let mut restrictive = Vec::new();
    let mut has_permissive = false;
    // A literally-true permissive predicate makes the permissive OR true
    // regardless of other permissive conditions. Track it separately so the
    // conjunct-building step knows to skip the permissive side entirely when
    // any one permissive policy grants all rows. Without this tracking, a
    // FOR SELECT USING (true) alongside a FOR ALL USING (owner = 'alice')
    // drops the true predicate but keeps the (owner = 'alice') conjunct,
    // producing a view narrower than PostgreSQL's (which ORs them to true).
    let mut permissive_always_true = false;

    for policy in policies {
        let is_restrictive = policy.policy_type() == CreatePolicyType::Restrictive;
        if !is_restrictive {
            has_permissive = true;
        }
        let expr = match clause {
            PolicyClause::Using => policy.using_expression(schema),
            // PostgreSQL: "If no WITH CHECK expression is defined, then the
            // USING expression will be used both to determine which rows are
            // visible and which new rows will be allowed to be added." So an
            // UPDATE that moves a row out of the USING predicate is rejected.
            //
            // The policy set reaching this arm is filtered to INSERT, UPDATE,
            // and ALL commands, so a SELECT or DELETE policy's USING can never
            // leak into a write guard.
            PolicyClause::Check => {
                policy.check_expression(schema).or_else(|| policy.using_expression(schema))
            }
        };
        let Some(expr) = expr else { continue };
        let mut transformed =
            transform_expr(expr, options, table, schema, prefix, table_rename, facts);
        // Folded before translation so a substituted default travels the same
        // path the column definition sends it down, and lands as the same SQL
        // the backing table declares.
        fold_guard(&mut transformed, substitutions);
        // The transformer above owns the RLS rewrites: session variables, NEW
        // and OLD prefixes, backing table renames. The forward expression
        // translator then owns the PostgreSQL to SQLite semantics, so a policy
        // cannot smuggle an untranslated operator or function into the emitted
        // view or trigger guard, where it would fail at apply time (ILIKE) or
        // lie dormant until the first read (now, date_trunc).
        let transformed = transformed.translate(schema, options)?;
        // A literally-true predicate constrains nothing. For a permissive
        // policy this means the permissive OR is satisfied for every row, so
        // we record that and skip adding any conjunct for it. For a restrictive
        // policy a true predicate constrains nothing and can also be dropped.
        if is_true_literal(&transformed) {
            if !is_restrictive {
                permissive_always_true = true;
            }
            continue;
        }
        if is_restrictive {
            restrictive.push(format!("({transformed})"));
        } else {
            permissive.push(format!("({transformed})"));
        }
    }

    if !has_permissive {
        return Ok(PolicyPredicate::DenyAll);
    }

    // Each condition is already parenthesised, so a grouping paren is only
    // needed when several permissive conditions are ORed together AND a
    // restrictive conjunct follows. Adding it unconditionally would churn every
    // snapshot with semantically identical output.
    let mut conjuncts = Vec::new();
    // When any permissive policy is literally true, the OR of all permissive
    // conditions is true. No permissive conjunct is needed and the collected
    // non-true permissive predicates are vacuous (true OR anything = true).
    if !permissive_always_true && !permissive.is_empty() {
        let joined = permissive.join(" OR ");
        if permissive.len() > 1 && !restrictive.is_empty() {
            conjuncts.push(format!("({joined})"));
        } else {
            conjuncts.push(joined);
        }
    }
    conjuncts.extend(restrictive);

    if conjuncts.is_empty() {
        Ok(PolicyPredicate::AllowAll)
    } else {
        Ok(PolicyPredicate::Expr(conjuncts.join(" AND ")))
    }
}

fn collect_column_names(
    table: &CreateTable,
    schema: &ParserDB,
) -> Result<Vec<String>, LookupError> {
    Ok(table.columns(schema)?.map(|c| c.column_name().to_string()).collect())
}

fn collect_pk_column_names(
    table: &CreateTable,
    schema: &ParserDB,
) -> Result<Vec<String>, LookupError> {
    Ok(table.primary_key_columns(schema)?.map(|c| c.column_name().to_string()).collect())
}

/// Builds the write-guard predicate context for one DML event.
///
/// Returns `(columns, using, check)` where `using` is `None` for INSERT
/// because PostgreSQL's RLS model has no USING clause on that command.
fn build_write_guard(
    policies: &[&CreatePolicy],
    kind: GuardKind,
    table: &CreateTable,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<(Vec<TriggerColumn>, Option<PolicyPredicate>, PolicyPredicate), Error> {
    let columns = trigger_columns(table, schema, options)?;
    let subs = guard_substitutions(&columns, kind, options, table, schema)?;
    let using = if kind == GuardKind::Update {
        Some(combine_policy_predicates(
            policies,
            PolicyClause::Using,
            Some("OLD"),
            options,
            table,
            schema,
            &[],
        )?)
    } else {
        None
    };
    let check = combine_policy_predicates(
        policies,
        PolicyClause::Check,
        Some("NEW"),
        options,
        table,
        schema,
        &subs,
    )?;
    Ok((columns, using, check))
}

/// Returns true if the table has RLS enabled.
///
/// # Errors
///
/// Returns [`LookupError`] if the table is present but its metadata cannot be
/// resolved in `schema`.
pub fn table_has_rls(table_name: &str, schema: &ParserDB) -> Result<bool, LookupError> {
    match schema.table(None, table_name) {
        Some(table) => table.has_row_level_security(schema),
        None => Ok(false),
    }
}

/// Resolves the correct table name for attaching AFTER triggers.
///
/// When a table has RLS, it's split into a view and a backing table.
/// AFTER triggers must be attached to the backing table (e.g., `table_rls`),
/// not the view, because views don't fire AFTER triggers in SQLite.
///
/// # Errors
///
/// Returns [`LookupError`] if the table's metadata cannot be resolved in
/// `schema`.
pub fn resolve_trigger_table_name(
    base_name: &str,
    table: &CreateTable,
    schema: &ParserDB,
    options: &impl TranslationOptions,
) -> Result<String, LookupError> {
    if table.has_row_level_security(schema)? {
        let suffix = options.get_rls_table_suffix();
        Ok(format!("{base_name}{suffix}"))
    } else {
        Ok(base_name.to_string())
    }
}

/// Builds a WHERE clause for row identity using primary key columns if
/// available, otherwise falls back to all columns.
///
/// The keyless fallback uses `IS NOT DISTINCT FROM` rather than `=` because a
/// primary key cannot be NULL (so `=` is sufficient there) while any other
/// column can be. `NULL = NULL` evaluates to NULL, which is never true, so the
/// trigger's forwarding query silently matches zero rows. `IS NOT DISTINCT
/// FROM` treats two NULLs as equal and is otherwise identical to `=`. Keeping
/// `=` on the primary-key path avoids changing stored snapshots, since that
/// path cannot involve NULL.
fn build_row_identity_clause(columns: &[String], pk_columns: &[String]) -> String {
    let use_null_safe = pk_columns.is_empty();
    let identity_cols = if use_null_safe { columns } else { pk_columns };
    identity_cols
        .iter()
        .map(|c| {
            let col = quote_identifier(c);
            let old_col = prefixed_quoted_identifier("OLD", c);
            if use_null_safe {
                format!("{col} IS NOT DISTINCT FROM {old_col}")
            } else {
                format!("{col} = {old_col}")
            }
        })
        .collect::<Vec<_>>()
        .join(" AND ")
}

/// Returns the policies that apply to the given commands, filtered by the
/// session user role when a non-PUBLIC `TO` clause is present.
///
/// A policy applies when its `TO` list is empty (PUBLIC), contains the literal
/// `PUBLIC` role, or contains the configured session user role (exact name
/// match; no membership graph is consulted). When a policy has a non-PUBLIC
/// role restriction and no session user role is configured with
/// `with_session_user_role`, translation is refused so the caller does not
/// silently apply a policy to the wrong audience.
fn filter_policies<'a>(
    table: &'a CreateTable,
    schema: &'a ParserDB,
    commands: &[CreatePolicyCommand],
    options: &Pg2SqliteOptions,
) -> Result<Vec<&'a CreatePolicy>, Error> {
    let session_role = options.get_session_user_role();
    let mut policies = Vec::new();
    for policy in table.policies(schema)? {
        let command = policy.command();
        if !commands.contains(&command) && command != CreatePolicyCommand::All {
            continue;
        }
        // Determine whether this policy applies given its TO clause.
        let mut applies = false;
        let mut has_non_public_role = false;
        let roles: Vec<_> = policy.roles(schema).collect();
        if roles.is_empty() {
            // No TO clause means PUBLIC.
            applies = true;
        } else {
            for owner in &roles {
                if let Owner::Ident(ident) = owner {
                    if ident.value.eq_ignore_ascii_case("PUBLIC") {
                        applies = true;
                    } else {
                        has_non_public_role = true;
                        if session_role.is_some_and(|role| ident.value == role) {
                            applies = true;
                        }
                    }
                }
            }
        }
        if has_non_public_role && session_role.is_none() {
            return Err(Error::UnsupportedSQLiteFeature(format!(
                "policy '{}' on table '{}' is scoped to a specific role. \
                 Call with_session_user_role to specify which role this translation targets.",
                policy.name(),
                table.table_name(),
            )));
        }
        if applies {
            policies.push(policy);
        }
    }
    Ok(policies)
}

struct RlsTriggerContext<'a> {
    table_name: &'a str,
    inner_table_name: String,
}

impl<'a> RlsTriggerContext<'a> {
    fn new(table: &'a CreateTable, options: &Pg2SqliteOptions) -> Self {
        let table_name = table.table_name();
        let inner_table_name = format!("{}{}", table_name, options.get_rls_table_suffix());
        Self { table_name, inner_table_name }
    }

    const fn as_rename_tuple(&self) -> (&str, &str) {
        (self.table_name, self.inner_table_name.as_str())
    }
}

fn push_pattern_unique(
    patterns: &mut Vec<SessionVariablePattern>,
    pattern: SessionVariablePattern,
) {
    if !patterns.contains(&pattern) {
        patterns.push(pattern);
    }
}

fn collect_patterns_from_function(func: &Function, patterns: &mut Vec<SessionVariablePattern>) {
    if let Some(pattern) = session_variable::pattern_of_function(func) {
        push_pattern_unique(patterns, pattern);
    }

    if let FunctionArguments::List(arg_list) = &func.args {
        for arg in &arg_list.args {
            match arg {
                FunctionArg::Unnamed(FunctionArgExpr::Expr(expr))
                | FunctionArg::Named { arg: FunctionArgExpr::Expr(expr), .. }
                | FunctionArg::ExprNamed { arg: FunctionArgExpr::Expr(expr), .. } => {
                    collect_session_variable_patterns(expr, patterns);
                }
                _ => {}
            }
        }
    }

    if let Some(filter) = &func.filter {
        collect_session_variable_patterns(filter, patterns);
    }

    if let Some(over) = &func.over
        && let sqlparser::ast::WindowType::WindowSpec(window_spec) = over
    {
        for expr in &window_spec.partition_by {
            collect_session_variable_patterns(expr, patterns);
        }
        for order_by_expr in &window_spec.order_by {
            collect_session_variable_patterns(&order_by_expr.expr, patterns);
        }
    }

    for order_by_expr in &func.within_group {
        collect_session_variable_patterns(&order_by_expr.expr, patterns);
    }
}

fn collect_patterns_from_table_factor(
    factor: &TableFactor,
    patterns: &mut Vec<SessionVariablePattern>,
) {
    match factor {
        TableFactor::Derived { subquery, .. } => {
            collect_patterns_from_query(subquery, patterns);
        }
        TableFactor::NestedJoin { table_with_joins, .. } => {
            collect_patterns_from_table_factor(&table_with_joins.relation, patterns);
            for join in &table_with_joins.joins {
                collect_patterns_from_table_factor(&join.relation, patterns);
            }
        }
        _ => {}
    }
}

fn collect_patterns_from_select(
    select: &sqlparser::ast::Select,
    patterns: &mut Vec<SessionVariablePattern>,
) {
    for table_with_joins in &select.from {
        collect_patterns_from_table_factor(&table_with_joins.relation, patterns);
        for join in &table_with_joins.joins {
            collect_patterns_from_table_factor(&join.relation, patterns);
            if let Some(JoinConstraint::On(expr)) = join_constraint_ref(&join.join_operator) {
                collect_session_variable_patterns(expr, patterns);
            }
            if let JoinOperator::AsOf { match_condition, .. } = &join.join_operator {
                collect_session_variable_patterns(match_condition, patterns);
            }
        }
    }

    if let Some(selection) = &select.selection {
        collect_session_variable_patterns(selection, patterns);
    }
    if let Some(having) = &select.having {
        collect_session_variable_patterns(having, patterns);
    }
    if let Some(qualify) = &select.qualify {
        collect_session_variable_patterns(qualify, patterns);
    }

    for item in &select.projection {
        if let sqlparser::ast::SelectItem::UnnamedExpr(expr)
        | sqlparser::ast::SelectItem::ExprWithAlias { expr, .. } = item
        {
            collect_session_variable_patterns(expr, patterns);
        }
    }

    if let sqlparser::ast::GroupByExpr::Expressions(exprs, _) = &select.group_by {
        for expr in exprs {
            collect_session_variable_patterns(expr, patterns);
        }
    }
}

fn collect_patterns_from_set_expr(
    set_expr: &sqlparser::ast::SetExpr,
    patterns: &mut Vec<SessionVariablePattern>,
) {
    match set_expr {
        sqlparser::ast::SetExpr::Select(select) => collect_patterns_from_select(select, patterns),
        sqlparser::ast::SetExpr::Query(query) => collect_patterns_from_query(query, patterns),
        sqlparser::ast::SetExpr::SetOperation { left, right, .. } => {
            collect_patterns_from_set_expr(left, patterns);
            collect_patterns_from_set_expr(right, patterns);
        }
        sqlparser::ast::SetExpr::Values(values) => {
            for row in &values.rows {
                for expr in &row.content {
                    collect_session_variable_patterns(expr, patterns);
                }
            }
        }
        _ => {}
    }
}

fn collect_patterns_from_query(
    query: &sqlparser::ast::Query,
    patterns: &mut Vec<SessionVariablePattern>,
) {
    if let Some(with) = &query.with {
        for cte in &with.cte_tables {
            collect_patterns_from_query(&cte.query, patterns);
        }
    }
    collect_patterns_from_set_expr(query.body.as_ref(), patterns);

    if let Some(order_by) = &query.order_by
        && let sqlparser::ast::OrderByKind::Expressions(exprs) = &order_by.kind
    {
        for order_expr in exprs {
            collect_session_variable_patterns(&order_expr.expr, patterns);
        }
    }

    if let Some(limit_clause) = &query.limit_clause {
        match limit_clause {
            sqlparser::ast::LimitClause::LimitOffset { limit, offset, limit_by } => {
                if let Some(limit) = limit {
                    collect_session_variable_patterns(limit, patterns);
                }
                if let Some(offset) = offset {
                    collect_session_variable_patterns(&offset.value, patterns);
                }
                for expr in limit_by {
                    collect_session_variable_patterns(expr, patterns);
                }
            }
            sqlparser::ast::LimitClause::OffsetCommaLimit { offset, limit } => {
                collect_session_variable_patterns(offset, patterns);
                collect_session_variable_patterns(limit, patterns);
            }
        }
    }

    if let Some(fetch) = &query.fetch
        && let Some(quantity) = &fetch.quantity
    {
        collect_session_variable_patterns(quantity, patterns);
    }
}

/// Collect all session variable patterns used by an expression tree.
fn collect_session_variable_patterns(expr: &Expr, patterns: &mut Vec<SessionVariablePattern>) {
    match expr {
        Expr::Identifier(ident) if ident.value.eq_ignore_ascii_case("current_user") => {
            push_pattern_unique(patterns, SessionVariablePattern::CurrentUser);
        }
        Expr::Function(func) => collect_patterns_from_function(func, patterns),
        Expr::Subquery(query) | Expr::Exists { subquery: query, .. } => {
            collect_patterns_from_query(query, patterns);
        }
        Expr::InSubquery { expr: inner, subquery, .. } => {
            collect_session_variable_patterns(inner, patterns);
            collect_patterns_from_query(subquery, patterns);
        }
        _ => {
            for_each_child_expr(expr, &mut |child| {
                collect_session_variable_patterns(child, patterns);
            });
        }
    }
}

/// Validates that all session variable patterns in an expression have mappings
/// configured.
///
/// # Errors
///
/// Returns `Error::SessionVariableMappingNotFound` if a
/// `current_setting('name')` or `current_user` pattern is found in the
/// expression but no corresponding SQLite function mapping is configured.
pub fn validate_session_variables(
    expr: &Expr,
    options: &Pg2SqliteOptions,
    table_name: &str,
    policy_name: &str,
) -> Result<(), Error> {
    let mut patterns = Vec::new();
    collect_session_variable_patterns(expr, &mut patterns);

    for pattern in patterns {
        if options.find_session_variable(&pattern).is_none() {
            return Err(Error::SessionVariableMappingNotFound {
                pattern: match pattern {
                    SessionVariablePattern::CurrentUser => {
                        format!("current_user in table '{table_name}', policy '{policy_name}'")
                    }
                    SessionVariablePattern::CurrentSetting { name } => {
                        format!(
                            "current_setting('{name}') in table '{table_name}', policy '{policy_name}'"
                        )
                    }
                },
            });
        }
    }

    Ok(())
}

/// Validates that all policies for a table have required session variable
/// mappings configured.
///
/// # Errors
///
/// Returns `Error::SessionVariableMappingNotFound` if any policy contains
/// a session variable pattern without a corresponding SQLite function mapping.
pub fn validate_table_policies(
    table: &CreateTable,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<(), Error> {
    for policy in table.policies(schema)? {
        if let Some(using_expr) = policy.using_expression(schema) {
            validate_session_variables(using_expr, options, table.table_name(), policy.name())?;
        }
        if let Some(check_expr) = policy.check_expression(schema) {
            validate_session_variables(check_expr, options, table.table_name(), policy.name())?;
        }
    }
    Ok(())
}

/// Schema facts the expression transformer needs, resolved once by the caller.
///
/// Grouped so the transformer entry points take one reference rather than two
/// parallel slices that must always travel together.
#[derive(Clone, Copy)]
struct ResolvedSchemaFacts<'a> {
    /// The host table's column names, folded to lower case.
    lowercased_columns: &'a [String],
}

/// How column references are rewritten during expression transformation.
///
/// `prefix` qualifies bare column references, typically with `NEW` or `OLD`
/// depending on which row the surrounding trigger clause constrains. `None`
/// leaves them bare. `table_rename` rewrites a qualified reference to the
/// renamed backing table.
///
/// `lowercased_columns` holds the table's column names, folded to lower case
/// once by the caller. Resolving them up front keeps the recursive transformer
/// infallible and total, and avoids rescanning the table's columns at every
/// identifier node.
struct ColumnRefStrategy<'a> {
    prefix: Option<&'a str>,
    table_rename: Option<(&'a str, &'a str)>,
    lowercased_columns: &'a [String],
}

impl<'a> ColumnRefStrategy<'a> {
    fn table_rename(&self) -> Option<(&str, &str)> {
        self.table_rename
    }

    fn subquery_prefix(&self) -> Option<&str> {
        self.prefix
    }

    fn facts(&self) -> ResolvedSchemaFacts<'a> {
        ResolvedSchemaFacts { lowercased_columns: self.lowercased_columns }
    }

    fn has_column(&self, lowercased_name: &str) -> bool {
        self.lowercased_columns.iter().any(|column| column == lowercased_name)
    }
}

fn transform_function_argument_clause_rls(
    clause: &FunctionArgumentClause,
    options: &Pg2SqliteOptions,
    table: &CreateTable,
    schema: &ParserDB,
    strategy: &ColumnRefStrategy<'_>,
) -> FunctionArgumentClause {
    match clause {
        FunctionArgumentClause::OrderBy(order_by_exprs) => {
            FunctionArgumentClause::OrderBy(
                order_by_exprs
                    .iter()
                    .map(|ob| {
                        let mut transformed = ob.clone();
                        transformed.expr =
                            transform_expr_generic(&ob.expr, options, table, schema, strategy);
                        transformed
                    })
                    .collect(),
            )
        }
        FunctionArgumentClause::Limit(e) => {
            FunctionArgumentClause::Limit(transform_expr_generic(
                e, options, table, schema, strategy,
            ))
        }
        FunctionArgumentClause::Having(HavingBound(kind, e)) => {
            FunctionArgumentClause::Having(HavingBound(
                *kind,
                transform_expr_generic(e, options, table, schema, strategy),
            ))
        }
        FunctionArgumentClause::OnOverflow(ListAggOnOverflow::Truncate { filler, with_count }) => {
            FunctionArgumentClause::OnOverflow(ListAggOnOverflow::Truncate {
                filler: filler
                    .as_ref()
                    .map(|e| Box::new(transform_expr_generic(e, options, table, schema, strategy))),
                with_count: *with_count,
            })
        }
        other => other.clone(),
    }
}

fn transform_window_frame_bound_rls(
    bound: &sqlparser::ast::WindowFrameBound,
    options: &Pg2SqliteOptions,
    table: &CreateTable,
    schema: &ParserDB,
    strategy: &ColumnRefStrategy<'_>,
) -> sqlparser::ast::WindowFrameBound {
    match bound {
        sqlparser::ast::WindowFrameBound::Preceding(Some(e)) => {
            sqlparser::ast::WindowFrameBound::Preceding(Some(Box::new(transform_expr_generic(
                e, options, table, schema, strategy,
            ))))
        }
        sqlparser::ast::WindowFrameBound::Following(Some(e)) => {
            sqlparser::ast::WindowFrameBound::Following(Some(Box::new(transform_expr_generic(
                e, options, table, schema, strategy,
            ))))
        }
        other => other.clone(),
    }
}

#[allow(clippy::too_many_lines)]
fn transform_expr_generic(
    expr: &Expr,
    options: &Pg2SqliteOptions,
    table: &CreateTable,
    schema: &ParserDB,
    strategy: &ColumnRefStrategy<'_>,
) -> Expr {
    let recurse = |e: &Expr| transform_expr_generic(e, options, table, schema, strategy);

    match expr {
        // A cast over the caller's identity is left standing here and resolved
        // by the expression translator, which owns the mapping for every
        // statement kind. Doing it twice was how the type check the mapping
        // records came to apply to a query but not to a policy.
        Expr::Cast { expr: inner, data_type, format, kind } => {
            Expr::Cast {
                expr: Box::new(recurse(inner)),
                data_type: data_type.clone(),
                format: format.clone(),
                kind: kind.clone(),
            }
        }

        Expr::Function(func) => {
            let transformed_args = match &func.args {
                FunctionArguments::List(arg_list) => {
                    FunctionArguments::List(FunctionArgumentList {
                        duplicate_treatment: arg_list.duplicate_treatment,
                        args: arg_list
                            .args
                            .iter()
                            .map(|arg| transform_function_arg_with_rls(arg, &recurse))
                            .collect(),
                        clauses: arg_list
                            .clauses
                            .iter()
                            .map(|clause| {
                                transform_function_argument_clause_rls(
                                    clause, options, table, schema, strategy,
                                )
                            })
                            .collect(),
                    })
                }
                FunctionArguments::Subquery(query) => {
                    FunctionArguments::Subquery(Box::new(transform_query(
                        query,
                        options,
                        table,
                        schema,
                        strategy.subquery_prefix(),
                        strategy.table_rename(),
                        strategy.facts(),
                    )))
                }
                FunctionArguments::None => FunctionArguments::None,
            };

            let transformed_filter = func.filter.as_ref().map(|expr| Box::new(recurse(expr)));

            let transformed_over = func.over.as_ref().map(|window| {
                match window {
                    WindowType::WindowSpec(window_spec) => {
                        WindowType::WindowSpec(sqlparser::ast::WindowSpec {
                            window_name: window_spec.window_name.clone(),
                            partition_by: window_spec.partition_by.iter().map(&recurse).collect(),
                            order_by: window_spec
                                .order_by
                                .iter()
                                .map(|order_by_expr| {
                                    let mut transformed = order_by_expr.clone();
                                    transformed.expr = recurse(&order_by_expr.expr);
                                    transformed
                                })
                                .collect(),
                            window_frame: window_spec.window_frame.as_ref().map(|frame| {
                                sqlparser::ast::WindowFrame {
                                    units: frame.units,
                                    start_bound: transform_window_frame_bound_rls(
                                        &frame.start_bound,
                                        options,
                                        table,
                                        schema,
                                        strategy,
                                    ),
                                    end_bound: frame.end_bound.as_ref().map(|b| {
                                        transform_window_frame_bound_rls(
                                            b, options, table, schema, strategy,
                                        )
                                    }),
                                }
                            }),
                        })
                    }
                    WindowType::NamedWindow(named_window) => {
                        WindowType::NamedWindow(named_window.clone())
                    }
                }
            });

            let transformed_within_group = func
                .within_group
                .iter()
                .map(|order_by_expr| {
                    let mut transformed = order_by_expr.clone();
                    transformed.expr = recurse(&order_by_expr.expr);
                    transformed
                })
                .collect();

            let transformed_parameters = match &func.parameters {
                FunctionArguments::List(param_list) => {
                    FunctionArguments::List(FunctionArgumentList {
                        duplicate_treatment: param_list.duplicate_treatment,
                        args: param_list
                            .args
                            .iter()
                            .map(|arg| transform_function_arg_with_rls(arg, &recurse))
                            .collect(),
                        clauses: param_list
                            .clauses
                            .iter()
                            .map(|clause| {
                                transform_function_argument_clause_rls(
                                    clause, options, table, schema, strategy,
                                )
                            })
                            .collect(),
                    })
                }
                other => other.clone(),
            };

            Expr::Function(Function {
                name: func.name.clone(),
                uses_odbc_syntax: func.uses_odbc_syntax,
                parameters: transformed_parameters,
                args: transformed_args,
                filter: transformed_filter,
                null_treatment: func.null_treatment,
                over: transformed_over,
                within_group: transformed_within_group,
            })
        }

        // Handle bare column identifiers. `current_user` is not one: the
        // PostgreSQL dialect parses the keyword as a function, so an identifier
        // spelled that way came from a quoted `"current_user"`, which names a
        // column.
        Expr::Identifier(ident) => {
            let ident_lower = ident.value.to_lowercase();

            if let Some(pfx) = strategy.prefix
                && strategy.has_column(&ident_lower)
            {
                return Expr::CompoundIdentifier(vec![Ident::new(pfx), ident.clone()]);
            }

            Expr::Identifier(ident.clone())
        }

        // Handle already-qualified identifiers (e.g., table.column)
        Expr::CompoundIdentifier(idents) => {
            if let Some((old_name, new_name)) = strategy.table_rename()
                && idents.len() >= 2
                && idents[0].value.to_lowercase() == old_name.to_lowercase()
            {
                let mut new_idents = idents.clone();
                new_idents[0] = Ident::new(strategy.prefix.unwrap_or(new_name));
                return Expr::CompoundIdentifier(new_idents);
            }
            Expr::CompoundIdentifier(idents.clone())
        }

        // Handle subqueries with transform_query (not just expr recursion)
        Expr::Exists { subquery, negated } => {
            Expr::Exists {
                subquery: Box::new(transform_query(
                    subquery,
                    options,
                    table,
                    schema,
                    strategy.subquery_prefix(),
                    strategy.table_rename(),
                    strategy.facts(),
                )),
                negated: *negated,
            }
        }

        Expr::Subquery(subquery) => {
            Expr::Subquery(Box::new(transform_query(
                subquery,
                options,
                table,
                schema,
                strategy.subquery_prefix(),
                strategy.table_rename(),
                strategy.facts(),
            )))
        }

        Expr::InSubquery { expr: inner, subquery, negated } => {
            Expr::InSubquery {
                expr: Box::new(recurse(inner)),
                subquery: Box::new(transform_query(
                    subquery,
                    options,
                    table,
                    schema,
                    strategy.subquery_prefix(),
                    strategy.table_rename(),
                    strategy.facts(),
                )),
                negated: *negated,
            }
        }

        // All other variants: delegate structural recursion to map_expr_children
        other => map_expr_children(other, &recurse),
    }
}

fn transform_expr(
    expr: &Expr,
    options: &Pg2SqliteOptions,
    table: &CreateTable,
    schema: &ParserDB,
    prefix: Option<&str>,
    table_rename: Option<(&str, &str)>,
    facts: ResolvedSchemaFacts<'_>,
) -> Expr {
    transform_expr_generic(
        expr,
        options,
        table,
        schema,
        &ColumnRefStrategy { prefix, table_rename, lowercased_columns: facts.lowercased_columns },
    )
}

fn transform_query(
    query: &sqlparser::ast::Query,
    options: &Pg2SqliteOptions,
    table: &CreateTable,
    schema: &ParserDB,
    prefix: Option<&str>,
    outer_table: Option<(&str, &str)>,
    facts: ResolvedSchemaFacts<'_>,
) -> sqlparser::ast::Query {
    let mut transformed = query.clone();
    let rls_suffix = options.get_rls_table_suffix();
    let context = SubqueryTransformContext {
        options,
        table,
        schema,
        prefix,
        outer_table,
        rls_suffix,
        lowercased_columns: facts.lowercased_columns,
    };

    if let sqlparser::ast::SetExpr::Select(ref mut select) = *transformed.body {
        let mut subquery_table_renames: Vec<(String, String)> = Vec::new();
        for table_with_joins in &mut select.from {
            transform_table_with_joins_for_subquery(
                table_with_joins,
                &context,
                &mut subquery_table_renames,
            );
        }

        let rewrite_expr =
            |expr: &Expr| transform_subquery_expression(expr, &context, &subquery_table_renames);

        if let Some(selection) = &mut select.selection {
            *selection = rewrite_expr(selection);
        }

        for item in &mut select.projection {
            if let sqlparser::ast::SelectItem::UnnamedExpr(expr)
            | sqlparser::ast::SelectItem::ExprWithAlias { expr, .. } = item
            {
                *expr = rewrite_expr(expr);
            }
        }

        if let Some(having) = &mut select.having {
            *having = rewrite_expr(having);
        }

        if let Some(qualify) = &mut select.qualify {
            *qualify = rewrite_expr(qualify);
        }

        if let sqlparser::ast::GroupByExpr::Expressions(group_exprs, _) = &mut select.group_by {
            for group_expr in group_exprs {
                *group_expr = rewrite_expr(group_expr);
            }
        }
    }

    transformed
}

fn transform_subquery_expression(
    expr: &Expr,
    context: &SubqueryTransformContext<'_>,
    subquery_table_renames: &[(String, String)],
) -> Expr {
    let (options, table, schema, prefix) =
        (context.options, context.table, context.schema, context.prefix);
    let facts = context.facts();
    let mut transformed = expr.clone();

    if let Some((outer_table_name, renamed_table_name)) = context.outer_table {
        transformed = transform_outer_table_refs(
            &transformed,
            outer_table_name,
            prefix,
            Some(renamed_table_name),
        );
    }

    // Subquery table renames only rewrite table-name qualifiers; applying the
    // outer prefix here would turn `members.col` into `NEW.col`, which is
    // wrong when `members` is the subquery's own FROM table, not the guarded
    // outer table. The outer table was already handled by
    // `transform_outer_table_refs` above.
    for (old_name, new_name) in subquery_table_renames {
        transformed = transform_expr(
            &transformed,
            options,
            table,
            schema,
            None,
            Some((old_name.as_str(), new_name.as_str())),
            facts,
        );
    }

    transform_expr(&transformed, options, table, schema, prefix, None, facts)
}

struct SubqueryTransformContext<'a> {
    options: &'a Pg2SqliteOptions,
    table: &'a CreateTable,
    schema: &'a ParserDB,
    prefix: Option<&'a str>,
    outer_table: Option<(&'a str, &'a str)>,
    rls_suffix: &'a str,
    /// The host table's column names, lower-cased once by the caller.
    lowercased_columns: &'a [String],
}

impl<'a> SubqueryTransformContext<'a> {
    fn facts(&self) -> ResolvedSchemaFacts<'a> {
        ResolvedSchemaFacts { lowercased_columns: self.lowercased_columns }
    }
}

fn transform_table_with_joins_for_subquery(
    table_with_joins: &mut sqlparser::ast::TableWithJoins,
    context: &SubqueryTransformContext<'_>,
    subquery_table_renames: &mut Vec<(String, String)>,
) {
    transform_table_factor_for_subquery(
        &mut table_with_joins.relation,
        context,
        subquery_table_renames,
    );

    for join in &mut table_with_joins.joins {
        transform_table_factor_for_subquery(&mut join.relation, context, subquery_table_renames);
        transform_join_operator_for_subquery(
            &mut join.join_operator,
            context,
            subquery_table_renames,
        );
    }
}

fn transform_table_factor_for_subquery(
    factor: &mut TableFactor,
    context: &SubqueryTransformContext<'_>,
    subquery_table_renames: &mut Vec<(String, String)>,
) {
    match factor {
        TableFactor::Table { name, .. } => {
            let old_name =
                last_ident(name).map_or_else(|| name.to_string(), |ident| ident.value.clone());
            if old_name.ends_with(context.rls_suffix) {
                subquery_table_renames.push((old_name.clone(), old_name));
                return;
            }

            // Every reference keeps the name the policy wrote, so a second
            // guarded table is read through its view and its own policy filters
            // what this subquery sees, which is how PostgreSQL evaluates it.
            // Reading the backing table instead would bypass that policy, and
            // whether the reference carries an alias cannot be what decides it.
            // The rename entry is an identity so the qualifier rewrite in
            // `transform_subquery_expression` leaves the reference alone.
            subquery_table_renames.push((old_name.clone(), old_name));
        }
        TableFactor::Derived { subquery, .. } => {
            **subquery = transform_query(
                subquery,
                context.options,
                context.table,
                context.schema,
                context.prefix,
                context.outer_table,
                context.facts(),
            );
        }
        TableFactor::NestedJoin { table_with_joins, .. } => {
            transform_table_with_joins_for_subquery(
                table_with_joins,
                context,
                subquery_table_renames,
            );
        }
        _ => {}
    }
}

fn transform_join_operator_for_subquery(
    join_operator: &mut JoinOperator,
    context: &SubqueryTransformContext<'_>,
    subquery_table_renames: &[(String, String)],
) {
    let rewrite_constraint = |constraint: &mut JoinConstraint| {
        if let JoinConstraint::On(expr) = constraint {
            *expr = transform_subquery_expression(expr, context, subquery_table_renames);
        }
    };

    if let Some(constraint) = join_constraint_mut(join_operator) {
        rewrite_constraint(constraint);
    }

    if let JoinOperator::AsOf { match_condition, .. } = join_operator {
        *match_condition =
            transform_subquery_expression(match_condition, context, subquery_table_renames);
    }
}

/// Transforms references to the outer table to use the prefix (OLD/NEW) or
/// rename.
///
/// - If prefix is Some("OLD") or Some("NEW"): `ownables.id` -> `OLD.id` or
///   `NEW.id`
/// - If prefix is None: `ownables.id` -> `ownables_rls.id` (using
///   renamed_table)
fn transform_function_arg_with(
    args: &FunctionArguments,
    transform_expr_fn: &impl Fn(&Expr) -> Expr,
) -> FunctionArguments {
    match args {
        FunctionArguments::List(arg_list) => {
            let transform_arg_expr = |arg_expr: &FunctionArgExpr| -> FunctionArgExpr {
                match arg_expr {
                    FunctionArgExpr::Expr(e) => FunctionArgExpr::Expr(transform_expr_fn(e)),
                    other => other.clone(),
                }
            };
            let transform_arg = |arg: &FunctionArg| -> FunctionArg {
                match arg {
                    FunctionArg::Named { name, arg, operator } => {
                        FunctionArg::Named {
                            name: name.clone(),
                            arg: transform_arg_expr(arg),
                            operator: operator.clone(),
                        }
                    }
                    FunctionArg::ExprNamed { name, arg, operator } => {
                        FunctionArg::ExprNamed {
                            name: name.clone(),
                            arg: transform_arg_expr(arg),
                            operator: operator.clone(),
                        }
                    }
                    FunctionArg::Unnamed(arg) => FunctionArg::Unnamed(transform_arg_expr(arg)),
                }
            };
            let transform_clause = |clause: &FunctionArgumentClause| -> FunctionArgumentClause {
                match clause {
                    FunctionArgumentClause::OrderBy(order_by_exprs) => {
                        FunctionArgumentClause::OrderBy(
                            order_by_exprs
                                .iter()
                                .map(|ob| {
                                    let mut t = ob.clone();
                                    t.expr = transform_expr_fn(&ob.expr);
                                    t
                                })
                                .collect(),
                        )
                    }
                    FunctionArgumentClause::Limit(e) => {
                        FunctionArgumentClause::Limit(transform_expr_fn(e))
                    }
                    FunctionArgumentClause::Having(HavingBound(kind, e)) => {
                        FunctionArgumentClause::Having(HavingBound(*kind, transform_expr_fn(e)))
                    }
                    other => other.clone(),
                }
            };
            FunctionArguments::List(FunctionArgumentList {
                duplicate_treatment: arg_list.duplicate_treatment,
                args: arg_list.args.iter().map(transform_arg).collect(),
                clauses: arg_list.clauses.iter().map(transform_clause).collect(),
            })
        }
        other => other.clone(),
    }
}

fn transform_function_arg_with_rls(
    arg: &FunctionArg,
    transform_fn: &impl Fn(&Expr) -> Expr,
) -> FunctionArg {
    let transform_arg_expr = |arg_expr: &FunctionArgExpr| -> FunctionArgExpr {
        match arg_expr {
            FunctionArgExpr::Expr(e) => FunctionArgExpr::Expr(transform_fn(e)),
            other => other.clone(),
        }
    };
    match arg {
        FunctionArg::Named { name, arg, operator } => {
            FunctionArg::Named {
                name: name.clone(),
                arg: transform_arg_expr(arg),
                operator: operator.clone(),
            }
        }
        FunctionArg::ExprNamed { name, arg, operator } => {
            FunctionArg::ExprNamed {
                name: name.clone(),
                arg: transform_arg_expr(arg),
                operator: operator.clone(),
            }
        }
        FunctionArg::Unnamed(arg) => FunctionArg::Unnamed(transform_arg_expr(arg)),
    }
}

fn transform_outer_table_refs(
    expr: &Expr,
    outer_table_name: &str,
    prefix: Option<&str>,
    renamed_table: Option<&str>,
) -> Expr {
    let recurse = |e: &Expr| transform_outer_table_refs(e, outer_table_name, prefix, renamed_table);

    match expr {
        Expr::CompoundIdentifier(idents) => {
            // Check if this is a reference to the outer table
            if idents.len() >= 2
                && idents[0].value.to_lowercase() == outer_table_name.to_lowercase()
            {
                let mut new_idents = idents.clone();
                if let Some(pfx) = prefix {
                    // In trigger context: ownables.id -> OLD.id or NEW.id
                    new_idents[0] = Ident::new(pfx);
                } else if let Some(renamed) = renamed_table {
                    // In view context: ownables.id -> ownables_rls.id
                    new_idents[0] = Ident::new(renamed);
                }
                return Expr::CompoundIdentifier(new_idents);
            }
            Expr::CompoundIdentifier(idents.clone())
        }

        Expr::Function(func) => {
            let transformed_args = transform_function_arg_with(&func.args, &recurse);
            Expr::Function(Function {
                name: func.name.clone(),
                args: transformed_args,
                filter: func.filter.as_ref().map(|e| Box::new(recurse(e))),
                null_treatment: func.null_treatment,
                over: func.over.clone(),
                within_group: func.within_group.clone(),
                parameters: func.parameters.clone(),
                uses_odbc_syntax: func.uses_odbc_syntax,
            })
        }

        other => map_expr_children(other, &recurse),
    }
}

/// Computes the predicate the RLS view applies on the read path.
///
/// Single source of truth for whether a table's view denies every row, shared
/// by the view generator and the decision to emit validation triggers, so the
/// two cannot drift into disagreeing about the same table.
fn rls_read_predicate(
    table: &CreateTable,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<PolicyPredicate, Error> {
    let select_policies = filter_policies(table, schema, &[CreatePolicyCommand::Select], options)?;
    reject_self_referential_read_policy(&select_policies, table, schema, options)?;

    combine_policy_predicates(
        &select_policies,
        PolicyClause::Using,
        None,
        options,
        table,
        schema,
        &[],
    )
}

/// Collects the unqualified name of every table that appears in a direct
/// subquery FROM clause within `expr`, aliased or not. Both spellings are read
/// through the table's view, so both can close a cycle.
fn collect_subquery_tables(expr: &Expr, out: &mut Vec<String>) {
    match expr {
        Expr::Exists { subquery, .. } | Expr::Subquery(subquery) => {
            collect_query_tables(subquery, out);
        }
        Expr::InSubquery { subquery, expr: inner, .. } => {
            collect_subquery_tables(inner, out);
            collect_query_tables(subquery, out);
        }
        other => {
            for_each_child_expr(other, &mut |child| collect_subquery_tables(child, out));
        }
    }
}

fn collect_query_tables(query: &sqlparser::ast::Query, out: &mut Vec<String>) {
    if let sqlparser::ast::SetExpr::Select(select) = query.body.as_ref() {
        for twj in &select.from {
            collect_factor_tables(&twj.relation, out);
            for join in &twj.joins {
                collect_factor_tables(&join.relation, out);
            }
        }
        if let Some(sel) = &select.selection {
            collect_subquery_tables(sel, out);
        }
    }
}

fn collect_factor_tables(factor: &TableFactor, out: &mut Vec<String>) {
    if let TableFactor::Table { name, .. } = factor
        && let Some(last) = last_ident(name)
    {
        let name_lc = last.value.to_lowercase();
        if !out.iter().any(|r| r == &name_lc) {
            out.push(name_lc);
        }
    }
}

/// Refuses a read-path policy whose predicate reads the table it guards, or
/// where two tables' read policies read each other.
///
/// PostgreSQL cannot evaluate a self-referential policy. Reading the table
/// applies the policy and the policy reads the table, so it answers `infinite
/// recursion detected in policy for relation`, measured on PostgreSQL 17 as a
/// non-superuser for the plain, CTE and set-operation spellings alike. The
/// translated form is a view selecting from the backing table, so the same
/// predicate makes SQLite answer `view <table> is circularly defined` wherever
/// the inner reference is not renamed, and where it is renamed the view works
/// and filters, which accepts and evaluates input the source database refuses.
///
/// The same problem arises when table A's read policy reads table B and table
/// B's read policy reads table A. A policy reads another guarded table through
/// that table's view, so both views come to reference each other, which SQLite
/// rejects as a circular view definition the first time either one is queried.
///
/// Only the read path is refused, which is where PostgreSQL draws the line
/// too. A `WITH CHECK` predicate, and the `USING` predicate of an
/// `INSERT`-only, `UPDATE`-only or `DELETE`-only policy, all read the table
/// under its SELECT policy rather than their own, so nothing recurses and
/// PostgreSQL runs them. Those were measured and are left alone.
fn reject_self_referential_read_policy(
    policies: &[&CreatePolicy],
    table: &CreateTable,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<(), Error> {
    let guarded = table.table_name();
    for policy in policies {
        let Some(predicate) = policy.using.as_ref() else { continue };
        let reads_itself = sqlparser::ast::visit_relations(predicate, |relation| {
            if crate::impls::object_name::last_ident(relation)
                .is_some_and(|ident| ident.value.eq_ignore_ascii_case(guarded))
            {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        })
        .is_break();

        if reads_itself {
            return Err(Error::UnsupportedSQLiteFeature(format!(
                "The read policy {} on {guarded} reads {guarded} in its own USING predicate, \
                 which PostgreSQL cannot evaluate: reading the table applies the policy and the \
                 policy reads the table, so PostgreSQL answers `infinite recursion detected in \
                 policy for relation \"{guarded}\"`. Rewrite the predicate over another table, or \
                 restrict the policy to INSERT, UPDATE or DELETE, where PostgreSQL does evaluate \
                 a self reference.",
                policy.name
            )));
        }

        // Every table a policy reads is read through its own view, so any
        // reference here can close a cycle, aliased or not.
        let mut refs: Vec<String> = Vec::new();
        collect_subquery_tables(predicate, &mut refs);

        for other_name in &refs {
            if other_name.eq_ignore_ascii_case(guarded) {
                continue;
            }
            // Check whether the other table has its own read policy reading
            // back to the guarded table. If so, each view ends up defined in
            // terms of the other, which SQLite rejects at query time.
            let Some(other_table) =
                schema.rls_tables()?.find(|t| t.table_name().eq_ignore_ascii_case(other_name))
            else {
                continue;
            };
            let other_select =
                filter_policies(other_table, schema, &[CreatePolicyCommand::Select], options)?;
            for other_policy in &other_select {
                let Some(other_pred) = other_policy.using.as_ref() else { continue };
                let mut back_refs: Vec<String> = Vec::new();
                collect_subquery_tables(other_pred, &mut back_refs);
                if back_refs.iter().any(|r| r.eq_ignore_ascii_case(guarded)) {
                    return Err(Error::UnsupportedSQLiteFeature(format!(
                        "The read policy {} on {guarded} and the read policy {} on {other_name} \
                         read each other, so each view would be defined in terms of the other and \
                         SQLite would refuse both at query time. PostgreSQL answers `infinite \
                         recursion detected in policy for relation` for the same pair. Restructure \
                         one of the policies to read a table that is not guarded, or restrict it \
                         to INSERT, UPDATE or DELETE.",
                        policy.name, other_policy.name
                    )));
                }
            }
        }
    }
    Ok(())
}

/// Generates the CREATE VIEW SQL statement for a table with RLS.
///
/// # Errors
///
/// Infallible, but returns a `Result` to match the other RLS generators.
#[allow(clippy::unnecessary_wraps)]
pub fn generate_rls_view_sql(
    table: &CreateTable,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<String, Error> {
    let ctx = RlsTriggerContext::new(table, options);
    let table_name = ctx.table_name;
    let inner_table_name = &ctx.inner_table_name;
    let table_name_quoted = quote_identifier(table_name);
    let inner_table_name_quoted = quote_identifier(inner_table_name);

    let columns = collect_column_names(table, schema)?;
    let column_list =
        columns.iter().map(|column| quote_identifier(column)).collect::<Vec<_>>().join(", ");

    let where_clause = match rls_read_predicate(table, schema, options)? {
        // No permissive policy grants access, so no row is readable. Covers both
        // an empty policy set and a restrictive-only one.
        PolicyPredicate::DenyAll => " WHERE false".to_owned(),
        // Every applicable policy omitted its predicate, which is
        // permissive-true in PostgreSQL, so no filter is needed.
        PolicyPredicate::AllowAll => String::new(),
        PolicyPredicate::Expr(predicate) => format!(" WHERE {predicate}"),
    };

    Ok(format!(
        "CREATE VIEW {table_name_quoted} AS SELECT {column_list} FROM {inner_table_name_quoted}{where_clause}"
    ))
}

fn generate_insert_trigger_sql(
    table: &CreateTable,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<String, Error> {
    let ctx = RlsTriggerContext::new(table, options);
    let table_name = ctx.table_name;
    let inner_table_name = &ctx.inner_table_name;
    let table_name_quoted = quote_identifier(table_name);
    let inner_table_name_quoted = quote_identifier(inner_table_name);
    let trigger_name = quote_identifier(&format!("{table_name}_insert_trigger"));

    // Find INSERT policies
    let insert_policies = filter_policies(table, schema, &[CreatePolicyCommand::Insert], options)?;

    let (columns, _, check) =
        build_write_guard(&insert_policies, GuardKind::Insert, table, schema, options)?;
    let written: Vec<&TriggerColumn> =
        columns.iter().filter(|column| !column.is_generated()).collect();
    let column_list =
        written.iter().map(|column| quote_identifier(&column.name)).collect::<Vec<_>>().join(", ");
    let value_list = written
        .iter()
        .map(|column| Ok(column.forwarded_value().translate(schema, options)?.to_string()))
        .collect::<Result<Vec<_>, Error>>()?
        .join(", ");

    let refuse_computed = refuse_computed_writes(&columns, table_name, GuardKind::Insert);
    let forward = format!(
        "{refuse_computed}INSERT INTO {inner_table_name_quoted} ({column_list}) VALUES ({value_list});"
    );

    let trigger_body = if insert_policies.is_empty() {
        let message =
            sql_string_literal(&format!("permission denied: no INSERT policy on {table_name}"));
        let guard = write_guard_raise(&message, None, options);
        format!("BEGIN\n    {guard}\n    {forward}\nEND")
    } else {
        match check {
            PolicyPredicate::DenyAll => {
                let message = sql_string_literal(RLS_VIOLATION_ERROR);
                let guard = write_guard_raise(&message, None, options);
                format!("BEGIN\n    {guard}\n    {forward}\nEND")
            }
            PolicyPredicate::AllowAll => format!("BEGIN\n    {forward}\nEND"),
            PolicyPredicate::Expr(predicate) => {
                let message = sql_string_literal(RLS_VIOLATION_ERROR);
                let violation = format!("({predicate}) IS NOT TRUE");
                let guard = write_guard_raise(&message, Some(&violation), options);
                format!("BEGIN\n    {guard}\n    {forward}\nEND")
            }
        }
    };

    Ok(format!(
        "CREATE TRIGGER {trigger_name} INSTEAD OF INSERT ON {table_name_quoted} FOR EACH ROW {trigger_body}"
    ))
}

fn write_exemption_call(options: &Pg2SqliteOptions) -> Option<String> {
    options.get_write_exemption_function().map(|name| {
        let quoted = format!("\"{}\"", name.replace('"', "\"\""));
        format!("{quoted}()")
    })
}

/// Combines a policy violation with the configured fail-closed write exemption.
pub(crate) fn write_guard_condition(
    violation: Option<&str>,
    options: &Pg2SqliteOptions,
) -> Option<String> {
    let enforcement = write_exemption_call(options).map(|call| format!("({call}) IS NOT TRUE"));
    match (enforcement, violation) {
        (None, None) => None,
        (Some(enforcement), None) => Some(enforcement),
        (None, Some(violation)) => Some(violation.to_owned()),
        (Some(enforcement), Some(violation)) => Some(format!("{enforcement} AND ({violation})")),
    }
}

/// Formats the write guard as a trigger `WHEN` clause.
pub(crate) fn write_guard_when(violation: Option<&str>, options: &Pg2SqliteOptions) -> String {
    write_guard_condition(violation, options)
        .map_or_else(String::new, |condition| format!(" WHEN {condition}"))
}

fn policy_or_exemption(predicate: &str, options: &Pg2SqliteOptions) -> String {
    write_exemption_call(options)
        .map_or_else(|| predicate.to_owned(), |call| format!("({call}) IS TRUE OR ({predicate})"))
}

fn write_guard_raise(message: &str, violation: Option<&str>, options: &Pg2SqliteOptions) -> String {
    write_guard_condition(violation, options).map_or_else(
        || format!("SELECT RAISE(ABORT, {message});"),
        |condition| format!("SELECT RAISE(ABORT, {message}) WHERE {condition};"),
    )
}

/// Generates a BEFORE INSERT trigger on the backing table that enforces INSERT
/// policies.
fn generate_insert_check_trigger_sql(
    table: &CreateTable,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Option<String>, Error> {
    let ctx = RlsTriggerContext::new(table, options);
    let table_name = ctx.table_name;
    let inner_table_name = &ctx.inner_table_name;
    let inner_table_name_quoted = quote_identifier(inner_table_name);
    let trigger_name = quote_identifier(&format!("{inner_table_name}_insert_check"));

    let insert_policies = filter_policies(table, schema, &[CreatePolicyCommand::Insert], options)?;

    if insert_policies.is_empty() {
        let when = write_guard_when(None, options);
        return Ok(Some(format!(
            "CREATE TRIGGER {trigger_name} BEFORE INSERT ON {inner_table_name_quoted} FOR EACH ROW{when} \
             BEGIN SELECT RAISE(ABORT, 'permission denied: no INSERT policy on {table_name}'); END"
        )));
    }

    let (_, _, check) =
        build_write_guard(&insert_policies, GuardKind::Insert, table, schema, options)?;

    let trigger = match check {
        PolicyPredicate::DenyAll => {
            let when = write_guard_when(None, options);
            Some(format!(
                "CREATE TRIGGER {trigger_name} BEFORE INSERT ON {inner_table_name_quoted} FOR EACH ROW{when} \
                 BEGIN SELECT RAISE(ABORT, '{RLS_VIOLATION_ERROR}'); END"
            ))
        }
        PolicyPredicate::AllowAll => None,
        PolicyPredicate::Expr(predicate) => {
            let violation = format!("({predicate}) IS NOT TRUE");
            let when = write_guard_when(Some(&violation), options);
            Some(format!(
                "CREATE TRIGGER {trigger_name} BEFORE INSERT ON {inner_table_name_quoted} FOR EACH ROW{when} \
                 BEGIN SELECT RAISE(ABORT, '{RLS_VIOLATION_ERROR}'); END"
            ))
        }
    };
    Ok(trigger)
}

/// Generates a BEFORE UPDATE trigger on the **backing table** that enforces
/// UPDATE policies on the backing-table path (ON CONFLICT DO UPDATE redirect
/// and any raw backing UPDATE).
///
/// The view-path INSTEAD OF UPDATE trigger already filters by USING before
/// forwarding, and raises for WITH CHECK failures before the forwarding UPDATE
/// runs. So this guard never raises on the view path; it fires only when a
/// backing UPDATE is issued directly or via ON CONFLICT DO UPDATE.
///
/// Returns `None` when the combined predicate is AllowAll (no guard needed).
fn generate_update_check_trigger_sql(
    table: &CreateTable,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Option<String>, Error> {
    let ctx = RlsTriggerContext::new(table, options);
    let inner_table_name = &ctx.inner_table_name;
    let inner_table_name_quoted = quote_identifier(inner_table_name);
    let trigger_name = quote_identifier(&format!("{inner_table_name}_update_check"));

    let update_policies = filter_policies(table, schema, &[CreatePolicyCommand::Update], options)?;

    if update_policies.is_empty() {
        let when = write_guard_when(None, options);
        return Ok(Some(format!(
            "CREATE TRIGGER {trigger_name} BEFORE UPDATE ON {inner_table_name_quoted} FOR EACH ROW{when} \
             BEGIN SELECT RAISE(ABORT, '{RLS_VIOLATION_ERROR}'); END"
        )));
    }

    let (_, using_opt, check) =
        build_write_guard(&update_policies, GuardKind::Update, table, schema, options)?;
    let using = using_opt.expect("update guard always has a USING predicate");

    let trigger = match (using, check) {
        (PolicyPredicate::DenyAll, _) | (_, PolicyPredicate::DenyAll) => {
            let when = write_guard_when(None, options);
            Some(format!(
                "CREATE TRIGGER {trigger_name} BEFORE UPDATE ON {inner_table_name_quoted} FOR EACH ROW{when} \
                 BEGIN SELECT RAISE(ABORT, '{RLS_VIOLATION_ERROR}'); END"
            ))
        }
        (PolicyPredicate::AllowAll, PolicyPredicate::AllowAll) => None,
        (PolicyPredicate::AllowAll, PolicyPredicate::Expr(check_pred)) => {
            let violation = format!("({check_pred}) IS NOT TRUE");
            let when = write_guard_when(Some(&violation), options);
            Some(format!(
                "CREATE TRIGGER {trigger_name} BEFORE UPDATE ON {inner_table_name_quoted} FOR EACH ROW{when} \
                 BEGIN SELECT RAISE(ABORT, '{RLS_VIOLATION_ERROR}'); END"
            ))
        }
        (PolicyPredicate::Expr(using_pred), PolicyPredicate::AllowAll) => {
            let violation = format!("({using_pred}) IS NOT TRUE");
            let when = write_guard_when(Some(&violation), options);
            Some(format!(
                "CREATE TRIGGER {trigger_name} BEFORE UPDATE ON {inner_table_name_quoted} FOR EACH ROW{when} \
                 BEGIN SELECT RAISE(ABORT, '{RLS_VIOLATION_ERROR}'); END"
            ))
        }
        (PolicyPredicate::Expr(using_pred), PolicyPredicate::Expr(check_pred)) => {
            let violation = format!("({using_pred}) IS NOT TRUE OR ({check_pred}) IS NOT TRUE");
            let when = write_guard_when(Some(&violation), options);
            Some(format!(
                "CREATE TRIGGER {trigger_name} BEFORE UPDATE ON {inner_table_name_quoted} FOR EACH ROW{when} \
                 BEGIN SELECT RAISE(ABORT, '{RLS_VIOLATION_ERROR}'); END"
            ))
        }
    };
    Ok(trigger)
}

fn generate_delete_check_trigger_sql(
    table: &CreateTable,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Option<String>, Error> {
    if options.get_write_exemption_function().is_none() {
        return Ok(None);
    }
    let ctx = RlsTriggerContext::new(table, options);
    let inner_table_name = &ctx.inner_table_name;
    let inner_table_name_quoted = quote_identifier(inner_table_name);
    let trigger_name = quote_identifier(&format!("{inner_table_name}_delete_check"));
    let delete_policies = filter_policies(table, schema, &[CreatePolicyCommand::Delete], options)?;

    if delete_policies.is_empty() {
        let when = write_guard_when(None, options);
        return Ok(Some(format!(
            "CREATE TRIGGER {trigger_name} BEFORE DELETE ON {inner_table_name_quoted} FOR EACH ROW{when} \
             BEGIN SELECT RAISE(ABORT, '{RLS_VIOLATION_ERROR}'); END"
        )));
    }

    let using = combine_policy_predicates(
        &delete_policies,
        PolicyClause::Using,
        Some("OLD"),
        options,
        table,
        schema,
        &[],
    )?;
    Ok(match using {
        PolicyPredicate::DenyAll => {
            let when = write_guard_when(None, options);
            Some(format!(
                "CREATE TRIGGER {trigger_name} BEFORE DELETE ON {inner_table_name_quoted} FOR EACH ROW{when} \
                 BEGIN SELECT RAISE(ABORT, '{RLS_VIOLATION_ERROR}'); END"
            ))
        }
        PolicyPredicate::AllowAll => None,
        PolicyPredicate::Expr(predicate) => {
            let violation = format!("({predicate}) IS NOT TRUE");
            let when = write_guard_when(Some(&violation), options);
            Some(format!(
                "CREATE TRIGGER {trigger_name} BEFORE DELETE ON {inner_table_name_quoted} FOR EACH ROW{when} \
                 BEGIN SELECT RAISE(ABORT, '{RLS_VIOLATION_ERROR}'); END"
            ))
        }
    })
}

/// Generates INSTEAD OF UPDATE trigger SQL.
fn generate_update_trigger_sql(
    table: &CreateTable,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<String, Error> {
    let ctx = RlsTriggerContext::new(table, options);
    let table_name = ctx.table_name;
    let inner_table_name = &ctx.inner_table_name;
    let table_name_quoted = quote_identifier(table_name);
    let inner_table_name_quoted = quote_identifier(inner_table_name);
    let trigger_name = quote_identifier(&format!("{table_name}_update_trigger"));

    // Find UPDATE policies
    let update_policies = filter_policies(table, schema, &[CreatePolicyCommand::Update], options)?;

    // Get all column names for the SET clause
    let columns = collect_column_names(table, schema)?;

    // Get primary key columns
    let pk_columns = collect_pk_column_names(table, schema)?;

    // SQLite populates NEW fully in an INSTEAD OF UPDATE trigger: a column
    // absent from the statement's SET clause already carries its OLD value. So
    // assigning NEW.col forwards a partial update correctly, and it is the only
    // form that can store NULL when the caller asks for it. No default answers
    // for a NULL here, and none should: the row exists, so nothing is missing.
    // A computed column is the exception, assigned by nobody but SQLite.
    let (assigned, using_opt, check) =
        build_write_guard(&update_policies, GuardKind::Update, table, schema, options)?;
    let set_clause = assigned
        .iter()
        .filter(|column| !column.is_generated())
        .map(|column| {
            let quoted_column = quote_identifier(&column.name);
            let new_column = prefixed_quoted_identifier("NEW", &column.name);
            format!("{quoted_column} = {new_column}")
        })
        .collect::<Vec<_>>()
        .join(", ");

    // Build PK WHERE clause
    let pk_where = build_row_identity_clause(&columns, &pk_columns);

    let using = using_opt.expect("update guard always has a USING predicate");

    // USING selects which existing rows are updatable at all. A row that fails
    // it is skipped, not rejected: PostgreSQL leaves the statement affecting
    // zero rows. So the predicate appears twice, once narrowing the forwarded
    // UPDATE and once gating the WITH CHECK guard below.
    let using_predicate = match &using {
        PolicyPredicate::Expr(predicate) => Some(predicate.clone()),
        PolicyPredicate::AllowAll | PolicyPredicate::DenyAll => None,
    };
    let using_denies = update_policies.is_empty() || matches!(using, PolicyPredicate::DenyAll);
    let row_authorization = if using_denies {
        write_exemption_call(options).map(|call| format!("({call}) IS TRUE"))
    } else {
        using_predicate.as_deref().map(|predicate| policy_or_exemption(predicate, options))
    };
    let row_filter = row_authorization.as_ref().map_or_else(
        || pk_where.clone(),
        |authorization| format!("({pk_where}) AND ({authorization})"),
    );
    let refuse_computed = refuse_computed_writes(&assigned, table_name, GuardKind::Update);
    let forward = format!(
        "{refuse_computed}UPDATE {inner_table_name_quoted} SET {set_clause} WHERE {row_filter};"
    );

    let trigger_body = if using_denies {
        if options.is_strict_rls_write_deny() {
            let message =
                sql_string_literal(&format!("permission denied: no UPDATE policy on {table_name}"));
            let guard = write_guard_raise(&message, None, options);
            format!("BEGIN\n    {guard}\n    {forward}\nEND")
        } else if let Some(call) = write_exemption_call(options) {
            format!(
                "BEGIN\n    SELECT RAISE(IGNORE) WHERE ({call}) IS NOT TRUE;\n    {forward}\nEND"
            )
        } else {
            "BEGIN\n    SELECT NULL;\nEND".to_owned()
        }
    } else if matches!(check, PolicyPredicate::DenyAll) {
        let message = sql_string_literal(RLS_VIOLATION_ERROR);
        let guard = write_guard_raise(&message, None, options);
        format!("BEGIN\n    {guard}\n    {forward}\nEND")
    } else if let PolicyPredicate::Expr(check_predicate) = check {
        let violation = match &using_predicate {
            Some(predicate) => {
                format!("({predicate}) AND ({check_predicate}) IS NOT TRUE")
            }
            None => format!("({check_predicate}) IS NOT TRUE"),
        };
        let message = sql_string_literal(RLS_VIOLATION_ERROR);
        let guard = write_guard_raise(&message, Some(&violation), options);
        format!("BEGIN\n    {guard}\n    {forward}\nEND")
    } else {
        format!("BEGIN\n    {forward}\nEND")
    };

    Ok(format!(
        "CREATE TRIGGER {trigger_name} INSTEAD OF UPDATE ON {table_name_quoted} FOR EACH ROW {trigger_body}"
    ))
}

/// Generates INSTEAD OF DELETE trigger SQL.
fn generate_delete_trigger_sql(
    table: &CreateTable,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<String, Error> {
    let ctx = RlsTriggerContext::new(table, options);
    let table_name = ctx.table_name;
    let inner_table_name = &ctx.inner_table_name;
    let table_name_quoted = quote_identifier(table_name);
    let inner_table_name_quoted = quote_identifier(inner_table_name);
    let trigger_name = quote_identifier(&format!("{table_name}_delete_trigger"));

    // Find DELETE policies
    let delete_policies = filter_policies(table, schema, &[CreatePolicyCommand::Delete], options)?;

    // Get all column names for the WHERE clause fallback
    let columns = collect_column_names(table, schema)?;

    // Get primary key columns
    let pk_columns = collect_pk_column_names(table, schema)?;

    // Build PK WHERE clause
    let pk_where = build_row_identity_clause(&columns, &pk_columns);

    // USING selects which existing rows may be deleted, so it resolves against
    // OLD. DELETE has no WITH CHECK clause.
    let using = combine_policy_predicates(
        &delete_policies,
        PolicyClause::Using,
        Some("OLD"),
        options,
        table,
        schema,
        &[],
    )?;

    let using_denies = delete_policies.is_empty() || matches!(using, PolicyPredicate::DenyAll);
    let row_authorization = if using_denies {
        write_exemption_call(options).map(|call| format!("({call}) IS TRUE"))
    } else {
        match &using {
            PolicyPredicate::Expr(predicate) => Some(policy_or_exemption(predicate, options)),
            PolicyPredicate::AllowAll | PolicyPredicate::DenyAll => None,
        }
    };
    let row_filter = row_authorization.as_ref().map_or_else(
        || pk_where.clone(),
        |authorization| format!("({pk_where}) AND ({authorization})"),
    );
    let forward = format!("DELETE FROM {inner_table_name_quoted} WHERE {row_filter};");

    let trigger_body = if using_denies {
        if options.is_strict_rls_write_deny() {
            let message =
                sql_string_literal(&format!("permission denied: no DELETE policy on {table_name}"));
            let guard = write_guard_raise(&message, None, options);
            format!("BEGIN\n    {guard}\n    {forward}\nEND")
        } else if let Some(call) = write_exemption_call(options) {
            format!(
                "BEGIN\n    SELECT RAISE(IGNORE) WHERE ({call}) IS NOT TRUE;\n    {forward}\nEND"
            )
        } else {
            "BEGIN\n    SELECT NULL;\nEND".to_owned()
        }
    } else {
        format!("BEGIN\n    {forward}\nEND")
    };

    Ok(format!(
        "CREATE TRIGGER {trigger_name} INSTEAD OF DELETE ON {table_name_quoted} FOR EACH ROW {trigger_body}"
    ))
}

fn generate_readonly_backing_guard_sql(
    table: &CreateTable,
    options: &Pg2SqliteOptions,
) -> Vec<String> {
    if options.get_write_exemption_function().is_none() {
        return Vec::new();
    }
    let ctx = RlsTriggerContext::new(table, options);
    let inner_table_name_quoted = quote_identifier(&ctx.inner_table_name);
    let when = write_guard_when(None, options);
    let message = sql_string_literal(&format!(
        "permission denied: {} is read-only for this role",
        ctx.table_name
    ));
    [
        ("insert", "BEFORE INSERT"),
        ("update", "BEFORE UPDATE"),
        ("delete", "BEFORE DELETE"),
    ]
    .into_iter()
    .map(|(verb, event)| {
        let trigger_name =
            quote_identifier(&format!("{}_{}_check", ctx.inner_table_name, verb));
        format!(
            "CREATE TRIGGER {trigger_name} {event} ON {inner_table_name_quoted} FOR EACH ROW{when} \
             BEGIN SELECT RAISE(ABORT, {message}); END"
        )
    })
    .collect()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RlsStatementMode {
    ReadWrite,
    ReadOnly,
}

fn generate_rls_statements_with_mode(
    table: &CreateTable,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
    mode: RlsStatementMode,
) -> Result<Vec<Statement>, Error> {
    // Validate that audit table name is configured
    let audit_table_name =
        options.get_rls_audit_table_name().ok_or(Error::RlsAuditTableNameRequired)?;

    let dialect = sqlparser::dialect::SQLiteDialect {};
    let mut statements = Vec::new();

    // Generate view
    let view_sql = generate_rls_view_sql(table, schema, options)?;
    let view_context = match mode {
        RlsStatementMode::ReadWrite => "Failed to parse generated RLS view SQL",
        RlsStatementMode::ReadOnly => "Failed to parse generated read-only RLS view",
    };
    let view_stmts = parse_generated_sql(&dialect, &view_sql, view_context)?;
    statements.extend(view_stmts);

    if mode == RlsStatementMode::ReadWrite {
        // Generate INSERT trigger
        let insert_sql = generate_insert_trigger_sql(table, schema, options)?;
        let insert_stmts = parse_generated_sql(
            &dialect,
            &insert_sql,
            "Failed to parse generated RLS INSERT trigger SQL",
        )?;
        statements.extend(insert_stmts);

        // Backing-table BEFORE INSERT guard: emitted in both monitor and strict
        // mode so raw backing writes (RETURNING redirect, ON CONFLICT redirect,
        // direct insert) are also covered. Skipped when AllowAll (no guard needed).
        if let Some(check_sql) = generate_insert_check_trigger_sql(table, schema, options)? {
            let check_stmts = parse_generated_sql(
                &dialect,
                &check_sql,
                "Failed to parse generated RLS backing-table BEFORE INSERT guard SQL",
            )?;
            statements.extend(check_stmts);
        }

        // Backing-table BEFORE UPDATE guard: covers ON CONFLICT DO UPDATE and
        // any direct backing UPDATE. The view-path INSTEAD OF UPDATE trigger
        // already filters by USING before forwarding, so this guard never raises
        // on the view path. Skipped when AllowAll (no guard needed).
        if let Some(update_check_sql) = generate_update_check_trigger_sql(table, schema, options)? {
            let update_check_stmts = parse_generated_sql(
                &dialect,
                &update_check_sql,
                "Failed to parse generated RLS backing-table BEFORE UPDATE guard SQL",
            )?;
            statements.extend(update_check_stmts);
        }

        if let Some(delete_check_sql) = generate_delete_check_trigger_sql(table, schema, options)? {
            let delete_check_stmts = parse_generated_sql(
                &dialect,
                &delete_check_sql,
                "Failed to parse generated RLS backing-table BEFORE DELETE guard SQL",
            )?;
            statements.extend(delete_check_stmts);
        }

        // Generate INSTEAD OF UPDATE trigger
        let update_sql = generate_update_trigger_sql(table, schema, options)?;
        let update_stmts = parse_generated_sql(
            &dialect,
            &update_sql,
            "Failed to parse generated RLS UPDATE trigger SQL",
        )?;
        statements.extend(update_stmts);

        // Generate INSTEAD OF DELETE trigger
        let delete_sql = generate_delete_trigger_sql(table, schema, options)?;
        let delete_stmts = parse_generated_sql(
            &dialect,
            &delete_sql,
            "Failed to parse generated RLS DELETE trigger SQL",
        )?;
        statements.extend(delete_stmts);
    } else {
        for guard_sql in generate_readonly_backing_guard_sql(table, options) {
            let guard_stmts = parse_generated_sql(
                &dialect,
                &guard_sql,
                "Failed to parse generated read-only RLS backing guard SQL",
            )?;
            statements.extend(guard_stmts);
        }
    }

    // A deny-all view makes the monitor useless rather than strict: its check asks
    // whether a backing row is visible through the view, which is always no here,
    // so it would flag every write and distinguish nothing. Report the
    // configuration once now instead of once per row at runtime.
    if matches!(rls_read_predicate(table, schema, options)?, PolicyPredicate::DenyAll) {
        crate::warnings::emit(crate::warnings::TranslationWarning::RlsDeniesEveryRow {
            table: table.table_name().to_owned(),
        });
    } else {
        let validation_stmts =
            generate_rls_validation_statements(table, schema, options, audit_table_name)?;
        statements.extend(validation_stmts);
    }

    Ok(statements)
}

/// Generates all RLS-related SQL statements for a table.
///
/// # Errors
///
/// Returns an error if the generated SQL cannot be parsed by the SQLite dialect
/// parser.
pub fn generate_rls_statements(
    table: &CreateTable,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Vec<Statement>, Error> {
    generate_rls_statements_with_mode(table, schema, options, RlsStatementMode::ReadWrite)
}

/// Generates a read-only RLS view and opt-in exemptible backing guards.
///
/// # Errors
///
/// Returns an error if the generated SQL cannot be parsed by the SQLite dialect
/// parser.
pub fn generate_readonly_rls_statements(
    table: &CreateTable,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Vec<Statement>, Error> {
    generate_rls_statements_with_mode(table, schema, options, RlsStatementMode::ReadOnly)
}

/// Renames a CREATE TABLE statement to use the inner table name for RLS.
/// Also updates any foreign key references to other RLS tables.
#[must_use]
pub fn rename_table_for_rls(
    create_table: &CreateTable,
    options: &Pg2SqliteOptions,
    _schema: &ParserDB,
) -> CreateTable {
    let suffix = options.get_rls_table_suffix();
    let mut renamed = create_table.clone();
    renamed.name = append_suffix(&renamed.name, suffix);

    renamed
}

/// Generates the SQL to create the RLS audit table.
#[must_use]
pub fn generate_audit_table_sql(audit_table_name: &str) -> String {
    let audit_table_name_quoted = quote_identifier(audit_table_name);
    format!(
        r"CREATE TABLE IF NOT EXISTS {audit_table_name_quoted} (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    table_name TEXT NOT NULL,
    violation_type TEXT NOT NULL,
    row_identifier TEXT NOT NULL,
    policy_name TEXT,
    detected_at TEXT NOT NULL,
    severity TEXT NOT NULL,
    details TEXT,
    reported_at TEXT
) STRICT"
    )
}

fn build_row_identifier_expr(pk_columns: &[String], prefix: &str) -> String {
    if pk_columns.is_empty() {
        return "'<no PK>'".to_string();
    }

    pk_columns
        .iter()
        .map(|col| {
            format!(
                "{} || quote({})",
                sql_string_literal(&format!("{col}=")),
                prefixed_quoted_identifier(prefix, col)
            )
        })
        .collect::<Vec<_>>()
        .join(" || ', ' || ")
}

fn generate_row_visibility_check(table_name: &str, pk_columns: &[String], prefix: &str) -> String {
    let table_name_quoted = quote_identifier(table_name);
    let where_clause = if pk_columns.is_empty() {
        // No PK - check all rows (will be slow but correct)
        "1=1".to_string()
    } else {
        pk_columns
            .iter()
            .map(|col| {
                format!(
                    "{table_name_quoted}.{} = {}",
                    quote_identifier(col),
                    prefixed_quoted_identifier(prefix, col)
                )
            })
            .collect::<Vec<_>>()
            .join(" AND ")
    };

    format!("EXISTS (SELECT 1 FROM {table_name_quoted} WHERE {where_clause})")
}

fn generate_monitoring_trigger_sql(
    table_name: &str,
    inner_table_name: &str,
    pk_columns: &[String],
    audit_table_name: &str,
    strict_mode: bool,
    operation: &str,
) -> String {
    let visibility_check = generate_row_visibility_check(table_name, pk_columns, "NEW");
    let row_identifier = build_row_identifier_expr(pk_columns, "NEW");
    // strict_mode influences severity only; enforcement is the job of the
    // BEFORE INSERT/UPDATE guards and the INSTEAD OF triggers, not this audit.
    let severity = if strict_mode { "error" } else { "warning" };
    let op_upper = operation.to_uppercase();
    let past_participle = if operation == "insert" { "inserted into" } else { "updated in" };
    let trigger_name = quote_identifier(&format!("{inner_table_name}_rls_monitor_{operation}"));
    let inner_table_name_quoted = quote_identifier(inner_table_name);
    let audit_table_name_quoted = quote_identifier(audit_table_name);
    let table_name_literal = sql_string_literal(table_name);
    // The SELECT policy governs visibility; name it honestly.
    let policy_name_literal = sql_string_literal("SELECT policy");
    let severity_literal = sql_string_literal(severity);
    // Honest wording: the row is in the backing table but not readable through
    // the view. PostgreSQL allows this for writes without RETURNING; this is
    // not a policy violation in the PostgreSQL sense.
    let details_literal = sql_string_literal(&format!(
        "Row {past_participle} backing table but not readable through the RLS view; \
         PostgreSQL allows this for writes without RETURNING"
    ));

    format!(
        r"CREATE TRIGGER {trigger_name}
AFTER {op_upper} ON {inner_table_name_quoted}
FOR EACH ROW
BEGIN
    -- Log rows that are in the backing table but not readable through the RLS view.
    -- PostgreSQL allows invisible writes when no RETURNING is used; this is audit only.
    INSERT INTO {audit_table_name_quoted} (
        table_name,
        violation_type,
        row_identifier,
        policy_name,
        detected_at,
        severity,
        details,
        reported_at
    )
    SELECT
        {table_name_literal},
        'select_not_visible',
        {row_identifier},
        {policy_name_literal},
        datetime('now'),
        {severity_literal},
        {details_literal},
        NULL
    WHERE NOT ({visibility_check});
END"
    )
}

fn generate_validation_view_sql(
    table_name: &str,
    inner_table_name: &str,
    columns: &[String],
    pk_columns: &[String],
) -> String {
    let table_name_quoted = quote_identifier(table_name);
    let inner_table_name_quoted = quote_identifier(inner_table_name);
    let validation_view_name = quote_identifier(&format!("{inner_table_name}_violations"));
    let column_list =
        columns.iter().map(|column| quote_identifier(column)).collect::<Vec<_>>().join(", ");

    // Build the WHERE clause to match rows by primary key (fall back to all
    // columns when no PK is defined - rare but correct).
    let match_columns = if pk_columns.is_empty() { columns } else { pk_columns };
    let pk_match = match_columns
        .iter()
        .map(|col| {
            let col_quoted = quote_identifier(col);
            format!("{inner_table_name_quoted}.{col_quoted} = {table_name_quoted}.{col_quoted}")
        })
        .collect::<Vec<_>>()
        .join(" AND ");

    format!(
        r"CREATE VIEW {validation_view_name} AS
SELECT {column_list}
FROM {inner_table_name_quoted}
WHERE NOT EXISTS (
    SELECT 1
    FROM {table_name_quoted}
    WHERE {pk_match}
)"
    )
}

/// Generates the complete set of RLS validation statements for a table.
///
/// # Errors
///
/// Returns an error if the generated SQL cannot be parsed.
pub fn generate_rls_validation_statements(
    table: &CreateTable,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
    audit_table_name: &str,
) -> Result<Vec<Statement>, Error> {
    let dialect = sqlparser::dialect::SQLiteDialect {};
    let mut statements = Vec::new();

    let table_name = table.table_name();
    let inner_table_name = format!("{}{}", table_name, options.get_rls_table_suffix());
    let pk_columns = collect_pk_column_names(table, schema)?;
    let all_columns = collect_column_names(table, schema)?;
    let strict_mode = options.is_strict_rls_validation();

    // Generate INSERT and UPDATE monitoring triggers
    for operation in &["insert", "update"] {
        let monitor_sql = generate_monitoring_trigger_sql(
            table_name,
            &inner_table_name,
            &pk_columns,
            audit_table_name,
            strict_mode,
            operation,
        );
        let error_context = format!("Failed to parse generated RLS {operation} monitoring trigger");
        let stmts = parse_generated_sql(&dialect, &monitor_sql, &error_context)?;
        statements.extend(stmts);
    }

    // Generate validation view
    let validation_view_sql =
        generate_validation_view_sql(table_name, &inner_table_name, &all_columns, &pk_columns);
    let view_stmts = parse_generated_sql(
        &dialect,
        &validation_view_sql,
        "Failed to parse generated RLS validation view",
    )?;
    statements.extend(view_stmts);

    Ok(statements)
}

/// Parses the audit table DDL into a Statement.
///
/// # Errors
///
/// Returns an error if the generated SQL cannot be parsed.
pub fn generate_rls_audit_table(audit_table_name: &str) -> Result<Statement, Error> {
    let dialect = sqlparser::dialect::SQLiteDialect {};
    let sql = generate_audit_table_sql(audit_table_name);

    parse_single_generated_sql(&dialect, &sql, "Failed to parse generated RLS audit table SQL")
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use sql_traits::{
        structs::ParserDB,
        traits::{ColumnLike, DatabaseLike, TableLike},
    };

    /// Resolves the column set the transformer needs, exactly as production
    /// callers do, so these tests exercise the same shapes.
    fn resolved_sets(table: &<ParserDB as DatabaseLike>::Table, schema: &ParserDB) -> Vec<String> {
        TableLike::columns(table, schema)
            .expect("columns must resolve")
            .map(|c| c.column_name().to_lowercase())
            .collect()
    }
    use sqlparser::{
        ast::{
            Expr, Function, FunctionArg, FunctionArgExpr, FunctionArgOperator,
            FunctionArgumentList, FunctionArguments, Ident, JoinConstraint, JoinOperator,
            ObjectName, ObjectNamePart, Query, SetExpr, Statement, TableFactor,
        },
        dialect::PostgreSqlDialect,
        parser::Parser,
    };

    use super::{
        ResolvedSchemaFacts, SubqueryTransformContext, filter_policies,
        generate_delete_trigger_sql, generate_insert_trigger_sql, generate_readonly_rls_statements,
        generate_rls_audit_table, generate_rls_statements, generate_rls_validation_statements,
        generate_update_trigger_sql, rename_table_for_rls, transform_expr,
        transform_join_operator_for_subquery, transform_query, transform_table_factor_for_subquery,
        validate_session_variables, validate_table_policies,
    };
    use crate::{
        impls::{function_helpers::single_quoted_literal, session_variable},
        prelude::{Pg2SqliteOptions, TranslationOptions},
        traits::translation_options::{SessionVariableMapping, SessionVariablePattern},
    };

    fn parse_statements(sql: &str) -> Vec<Statement> {
        Parser::parse_sql(&PostgreSqlDialect {}, sql).expect("sql should parse")
    }

    fn parse_query(sql: &str) -> Query {
        let stmt = parse_statements(sql).remove(0);
        let Statement::Query(query) = stmt else {
            panic!("expected query");
        };
        *query
    }

    fn parse_expr(sql: &str) -> Expr {
        Parser::new(&PostgreSqlDialect {})
            .try_with_sql(sql)
            .expect("sql should parse")
            .parse_expr()
            .expect("expression should parse")
    }

    fn schema_from_sql(sql: &str) -> ParserDB {
        ParserDB::from_statements(parse_statements(sql), "test".to_string())
            .expect("schema should build")
    }

    #[test]
    fn extract_helpers_cover_string_literal_and_current_setting_edge_paths() {
        assert_eq!(
            single_quoted_literal(&Expr::Value(sqlparser::ast::ValueWithSpan::from(
                sqlparser::ast::Value::SingleQuotedString("x".to_string()),
            ))),
            Some("x")
        );
        assert!(
            single_quoted_literal(&Expr::Value(sqlparser::ast::ValueWithSpan::from(
                sqlparser::ast::Value::Boolean(true),
            )))
            .is_none()
        );
        assert!(single_quoted_literal(&parse_expr("other_col")).is_none());

        let not_setting = Function {
            name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new("other"))]),
            uses_odbc_syntax: false,
            args: FunctionArguments::None,
            filter: None,
            null_treatment: None,
            over: None,
            within_group: vec![],
            parameters: FunctionArguments::None,
        };
        assert!(session_variable::pattern_of_function(&not_setting).is_none());

        let invalid_arg = Function {
            name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new("current_setting"))]),
            uses_odbc_syntax: false,
            args: FunctionArguments::List(FunctionArgumentList {
                duplicate_treatment: None,
                args: vec![FunctionArg::Named {
                    name: Ident::new("x"),
                    arg: FunctionArgExpr::Wildcard,
                    operator: FunctionArgOperator::RightArrow,
                }],
                clauses: vec![],
            }),
            filter: None,
            null_treatment: None,
            over: None,
            within_group: vec![],
            parameters: FunctionArguments::None,
        };
        assert!(session_variable::pattern_of_function(&invalid_arg).is_none());

        let named_expr = Function {
            args: FunctionArguments::List(FunctionArgumentList {
                duplicate_treatment: None,
                args: vec![FunctionArg::Named {
                    name: Ident::new("setting"),
                    arg: FunctionArgExpr::Expr(Expr::Value(sqlparser::ast::ValueWithSpan::from(
                        sqlparser::ast::Value::SingleQuotedString("app.user_id".to_string()),
                    ))),
                    operator: FunctionArgOperator::RightArrow,
                }],
                clauses: vec![],
            }),
            ..invalid_arg
        };
        assert_eq!(
            session_variable::pattern_of_function(&named_expr),
            Some(SessionVariablePattern::CurrentSetting { name: "app.user_id".to_string() })
        );

        let current_setting_no_args = Function {
            name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new("current_setting"))]),
            uses_odbc_syntax: false,
            args: FunctionArguments::None,
            filter: None,
            null_treatment: None,
            over: None,
            within_group: vec![],
            parameters: FunctionArguments::None,
        };
        assert!(session_variable::pattern_of_function(&current_setting_no_args).is_none());
    }

    /// The transformer owns column references and table renames. The caller's
    /// identity is not its business any more: the expression translator
    /// substitutes it for every statement kind, so a policy and a query cannot
    /// disagree about it.
    #[test]
    fn transform_expr_covers_cast_and_rename_paths() {
        let schema = schema_from_sql(
            "CREATE TABLE docs(id INTEGER PRIMARY KEY, owner_id INTEGER, title TEXT);",
        );
        let table = schema.table(None, "docs").expect("table should exist");
        let options = Pg2SqliteOptions::default().with_session_variable(
            crate::traits::translation_options::SessionVariableMapping::current_user("sqlite_user"),
        );
        let lowercased_columns = resolved_sets(table, &schema);
        let facts = ResolvedSchemaFacts { lowercased_columns: &lowercased_columns };

        let keyword = transform_expr(
            &parse_expr("current_user"),
            &options,
            table,
            &schema,
            Some("NEW"),
            None,
            facts,
        );
        assert_eq!(
            keyword.to_string(),
            "current_user",
            "the keyword is left for the translator, which is where the mapping now lives"
        );

        // PostgreSQL reads a quoted `"current_user"` as a column name, so the
        // transformer must not mistake it for the keyword. It is not a column of
        // this table, so it takes no prefix either.
        let quoted = transform_expr(
            &parse_expr("\"current_user\""),
            &options,
            table,
            &schema,
            Some("NEW"),
            None,
            facts,
        );
        assert_eq!(quoted.to_string(), "\"current_user\"");

        let transformed_cast = transform_expr(
            &parse_expr("owner_id::INT"),
            &options,
            table,
            &schema,
            Some("NEW"),
            None,
            facts,
        );
        assert!(transformed_cast.to_string().contains("NEW.owner_id"));

        let renamed = transform_expr(
            &parse_expr("docs.owner_id"),
            &options,
            table,
            &schema,
            None,
            Some(("docs", "docs_rls")),
            facts,
        );
        assert_eq!(renamed.to_string(), "docs_rls.owner_id");

        let prefixed_identifier = transform_expr(
            &parse_expr("owner_id"),
            &options,
            table,
            &schema,
            Some("NEW"),
            None,
            facts,
        );
        assert_eq!(prefixed_identifier.to_string(), "NEW.owner_id");
    }

    #[test]
    fn transform_query_and_subquery_helpers_cover_projection_and_join_paths() {
        let schema = schema_from_sql(
            r#"
            CREATE TABLE docs(id INTEGER PRIMARY KEY, owner_id INTEGER);
            ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
            CREATE POLICY docs_select ON docs FOR SELECT USING (owner_id > 0);
            CREATE TABLE teams(id INTEGER PRIMARY KEY, owner_id INTEGER);
            "#,
        );
        let table = schema.table(None, "docs").expect("table should exist");
        let options = Pg2SqliteOptions::default();
        let lowercased_columns = resolved_sets(table, &schema);
        let facts = ResolvedSchemaFacts { lowercased_columns: &lowercased_columns };

        let mut wildcard_query = parse_query("SELECT * FROM docs");
        let SetExpr::Select(select) = wildcard_query.body.as_mut() else {
            panic!("expected select");
        };
        select.qualify = Some(parse_expr("id > 0"));
        let transformed = transform_query(
            &wildcard_query,
            &options,
            table,
            &schema,
            Some("NEW"),
            Some(("docs", "docs_rls")),
            facts,
        );
        assert!(transformed.to_string().contains("QUALIFY"));

        let context = SubqueryTransformContext {
            options: &options,
            table,
            schema: &schema,
            prefix: Some("NEW"),
            outer_table: Some(("docs", "docs_rls")),
            rls_suffix: options.get_rls_table_suffix(),
            lowercased_columns: &lowercased_columns,
        };

        let mut rename_pairs = Vec::new();
        let mut already_suffixed = TableFactor::Table {
            name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new("docs_rls"))]),
            alias: None,
            args: None,
            with_hints: vec![],
            version: None,
            with_ordinality: false,
            partitions: vec![],
            json_path: None,
            sample: None,
            index_hints: vec![],
        };
        transform_table_factor_for_subquery(&mut already_suffixed, &context, &mut rename_pairs);
        assert_eq!(rename_pairs, vec![("docs_rls".to_string(), "docs_rls".to_string())]);

        let mut table_function =
            TableFactor::TableFunction { expr: parse_expr("generate_series(1, 2)"), alias: None };
        transform_table_factor_for_subquery(&mut table_function, &context, &mut rename_pairs);
        assert!(matches!(table_function, TableFactor::TableFunction { .. }));

        let on_constraint = JoinConstraint::On(parse_expr("docs.id = teams.id"));
        let mut join_variants = vec![
            JoinOperator::Join(on_constraint.clone()),
            JoinOperator::Inner(on_constraint.clone()),
            JoinOperator::Left(on_constraint.clone()),
            JoinOperator::LeftOuter(on_constraint.clone()),
            JoinOperator::Right(on_constraint.clone()),
            JoinOperator::RightOuter(on_constraint.clone()),
            JoinOperator::FullOuter(on_constraint.clone()),
            JoinOperator::CrossJoin(on_constraint.clone()),
            JoinOperator::Semi(on_constraint.clone()),
            JoinOperator::LeftSemi(on_constraint.clone()),
            JoinOperator::RightSemi(on_constraint.clone()),
            JoinOperator::Anti(on_constraint.clone()),
            JoinOperator::LeftAnti(on_constraint.clone()),
            JoinOperator::RightAnti(on_constraint.clone()),
            JoinOperator::StraightJoin(on_constraint.clone()),
        ];
        for join_op in &mut join_variants {
            transform_join_operator_for_subquery(join_op, &context, &rename_pairs);
        }

        let mut as_of = JoinOperator::AsOf {
            constraint: on_constraint.clone(),
            match_condition: parse_expr("docs.id > teams.id"),
        };
        transform_join_operator_for_subquery(&mut as_of, &context, &rename_pairs);
        assert!(matches!(as_of, JoinOperator::AsOf { .. }));

        let mut cross_apply = JoinOperator::CrossApply;
        transform_join_operator_for_subquery(&mut cross_apply, &context, &rename_pairs);
        let mut outer_apply = JoinOperator::OuterApply;
        transform_join_operator_for_subquery(&mut outer_apply, &context, &rename_pairs);
    }

    #[test]
    fn transform_table_factor_for_subquery_does_not_downgrade_three_part_names() {
        let schema = schema_from_sql(
            r#"
            CREATE TABLE docs(id INTEGER PRIMARY KEY, owner_id INTEGER);
            ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
            CREATE POLICY docs_select ON docs FOR SELECT USING (owner_id > 0);
            "#,
        );
        let table = schema.table(None, "docs").expect("table should exist");
        let options = Pg2SqliteOptions::default();
        let lowercased_columns = resolved_sets(table, &schema);
        let context = SubqueryTransformContext {
            options: &options,
            table,
            schema: &schema,
            prefix: Some("NEW"),
            outer_table: Some(("docs", "docs_rls")),
            rls_suffix: options.get_rls_table_suffix(),
            lowercased_columns: &lowercased_columns,
        };

        let mut rename_pairs = Vec::new();
        let mut three_part = TableFactor::Table {
            name: ObjectName(vec![
                ObjectNamePart::Identifier(Ident::new("catalog")),
                ObjectNamePart::Identifier(Ident::new("public")),
                ObjectNamePart::Identifier(Ident::new("docs")),
            ]),
            alias: None,
            args: None,
            with_hints: vec![],
            version: None,
            with_ordinality: false,
            partitions: vec![],
            json_path: None,
            sample: None,
            index_hints: vec![],
        };
        transform_table_factor_for_subquery(&mut three_part, &context, &mut rename_pairs);

        let TableFactor::Table { name, .. } = three_part else {
            panic!("expected Table variant");
        };
        assert_eq!(name.to_string(), "catalog.public.docs");
        assert_eq!(rename_pairs, vec![("docs".to_string(), "docs".to_string())]);
    }

    #[test]
    fn transform_expr_covers_prefix_and_rename_strategy_combinations() {
        let schema = schema_from_sql(
            "CREATE TABLE docs(id INTEGER PRIMARY KEY, owner_id INTEGER, body TEXT);",
        );
        let table = schema.table(None, "docs").expect("table should exist");
        let options = Pg2SqliteOptions::default();
        let lowercased_columns = resolved_sets(table, &schema);
        let facts = ResolvedSchemaFacts { lowercased_columns: &lowercased_columns };

        // Bare identifier, prefix applied.
        let prefixed_ident = transform_expr(
            &parse_expr("owner_id"),
            &options,
            table,
            &schema,
            Some("NEW"),
            None,
            facts,
        );
        assert_eq!(prefixed_ident.to_string(), "NEW.owner_id");

        // Qualified identifier, rename applied with no prefix.
        let renamed_compound = transform_expr(
            &parse_expr("docs.owner_id"),
            &options,
            table,
            &schema,
            None,
            Some(("docs", "docs_inner")),
            facts,
        );
        assert_eq!(renamed_compound.to_string(), "docs_inner.owner_id");

        // Qualified identifier with BOTH a prefix and a rename: the prefix wins,
        // which is the branch the collapsed strategy resolves via
        // `prefix.unwrap_or(new_name)`.
        let prefixed_over_rename = transform_expr(
            &parse_expr("docs.owner_id"),
            &options,
            table,
            &schema,
            Some("NEW"),
            Some(("docs", "docs_inner")),
            facts,
        );
        assert_eq!(prefixed_over_rename.to_string(), "NEW.owner_id");
    }

    #[test]
    fn transform_expr_identifier_and_compound_strategy_branches_are_exercised() {
        let schema = schema_from_sql(
            "CREATE TABLE docs(id INTEGER PRIMARY KEY, owner_id INTEGER, body TEXT);",
        );
        let table = schema.table(None, "docs").expect("table should exist");
        let options = Pg2SqliteOptions::default();
        let lowercased_columns = resolved_sets(table, &schema);
        let facts = ResolvedSchemaFacts { lowercased_columns: &lowercased_columns };

        let prefixed_identifier = transform_expr(
            &Expr::Identifier(Ident::new("owner_id")),
            &options,
            table,
            &schema,
            Some("OLD"),
            None,
            facts,
        );
        assert_eq!(prefixed_identifier.to_string(), "OLD.owner_id");

        let renamed_compound = transform_expr(
            &Expr::CompoundIdentifier(vec![Ident::new("docs"), Ident::new("owner_id")]),
            &options,
            table,
            &schema,
            None,
            Some(("docs", "docs_inner")),
            facts,
        );
        assert_eq!(renamed_compound.to_string(), "docs_inner.owner_id");

        let prefixed_compound = transform_expr(
            &Expr::CompoundIdentifier(vec![Ident::new("docs"), Ident::new("owner_id")]),
            &options,
            table,
            &schema,
            Some("OLD"),
            Some(("docs", "docs_inner")),
            facts,
        );
        assert_eq!(prefixed_compound.to_string(), "OLD.owner_id");
    }

    #[test]
    fn validate_session_variable_and_policy_paths_cover_error_and_success_cases() {
        let schema = schema_from_sql(
            r#"
            CREATE TABLE docs(id INTEGER PRIMARY KEY, owner_id INTEGER);
            ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
            CREATE POLICY docs_select ON docs FOR SELECT USING (owner_id = current_setting('app.user_id')::INT);
            "#,
        );
        let table = schema.table(None, "docs").expect("table should exist");

        let missing = Pg2SqliteOptions::default();
        let err = validate_session_variables(
            &parse_expr("owner_id = current_setting('app.user_id')::INT"),
            &missing,
            "docs",
            "docs_select",
        )
        .expect_err("missing current_setting mapping should error");
        assert!(err.to_string().contains("current_setting('app.user_id')"));

        let err = validate_session_variables(
            &parse_expr("current_user = 'alice'"),
            &missing,
            "docs",
            "docs_select",
        )
        .expect_err("missing current_user mapping should error");
        assert!(err.to_string().contains("current_user"));

        let mapped = Pg2SqliteOptions::default()
            .with_session_variable(SessionVariableMapping::current_setting(
                "app.user_id",
                "sqlite_user_id",
            ))
            .with_session_variable(SessionVariableMapping::current_user("sqlite_user"));
        validate_session_variables(
            &parse_expr("owner_id = current_setting('app.user_id')::INT"),
            &mapped,
            "docs",
            "docs_select",
        )
        .expect("mapped current_setting should pass");
        validate_session_variables(
            &parse_expr("current_user = 'alice'"),
            &mapped,
            "docs",
            "docs_select",
        )
        .expect("mapped current_user should pass");

        validate_table_policies(table, &schema, &mapped).expect("policy validation should pass");
    }

    #[test]
    fn query_and_trigger_generation_helpers_cover_policy_paths() -> Result<(), crate::errors::Error>
    {
        let schema = schema_from_sql(
            r#"
            CREATE TABLE docs(id INTEGER PRIMARY KEY, owner_id INTEGER, body TEXT);
            CREATE TABLE teams(id INTEGER PRIMARY KEY, owner_id INTEGER);
            ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
            CREATE POLICY docs_select ON docs FOR SELECT USING (owner_id > 0);
            CREATE POLICY docs_insert ON docs FOR INSERT WITH CHECK (owner_id > 0);
            CREATE POLICY docs_update ON docs FOR UPDATE USING (owner_id > 0) WITH CHECK (owner_id > 0);
            CREATE POLICY docs_delete ON docs FOR DELETE USING (owner_id > 0);
            "#,
        );
        let table = schema.table(None, "docs").expect("table should exist");
        let options = Pg2SqliteOptions::default()
            .with_rls_audit_table_name("rls_audit")
            .with_strict_rls_validation();
        let lowercased_columns = resolved_sets(table, &schema);
        let facts = ResolvedSchemaFacts { lowercased_columns: &lowercased_columns };

        let select_policies = filter_policies(
            table,
            &schema,
            &[sqlparser::ast::CreatePolicyCommand::Select],
            &options,
        )?;
        assert_eq!(select_policies.len(), 1);

        let transformed_query = transform_query(
            &parse_query(
                "SELECT docs.owner_id + 1 AS owner_plus \
                 FROM docs INNER JOIN teams ON docs.owner_id = teams.owner_id \
                 WHERE docs.owner_id > 0 \
                 GROUP BY docs.owner_id \
                 HAVING docs.owner_id > 1 \
                 QUALIFY docs.owner_id > 2",
            ),
            &options,
            table,
            &schema,
            Some("NEW"),
            Some(("docs", "docs_rls")),
            facts,
        );
        let transformed_sql = transformed_query.to_string();
        assert!(transformed_sql.contains("NEW.owner_id"));
        assert!(transformed_sql.contains("QUALIFY"));

        let insert_trigger_sql = generate_insert_trigger_sql(table, &schema, &options)?;
        assert!(insert_trigger_sql.contains("docs_insert_trigger"));
        assert!(insert_trigger_sql.contains("RAISE(ABORT"));

        let update_trigger_sql = generate_update_trigger_sql(table, &schema, &options)?;
        assert!(update_trigger_sql.contains("docs_update_trigger"));
        assert!(update_trigger_sql.contains("owner_id = NEW.owner_id"));
        assert!(
            !update_trigger_sql.contains("COALESCE"),
            "the SET clause must assign NEW.col directly, not COALESCE(NEW.col, OLD.col)"
        );

        let delete_trigger_sql = generate_delete_trigger_sql(table, &schema, &options)?;
        assert!(delete_trigger_sql.contains("docs_delete_trigger"));

        Ok(())
    }

    #[test]
    fn rls_statement_generation_paths_cover_readonly_and_validation_helpers() {
        let schema = schema_from_sql(
            r#"
            CREATE TABLE docs(id INTEGER PRIMARY KEY, owner_id INTEGER, body TEXT);
            ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
            CREATE POLICY docs_select ON docs FOR SELECT USING (owner_id > 0);
            CREATE POLICY docs_insert ON docs FOR INSERT WITH CHECK (owner_id > 0);
            CREATE POLICY docs_update ON docs FOR UPDATE USING (owner_id > 0) WITH CHECK (owner_id > 0);
            CREATE POLICY docs_delete ON docs FOR DELETE USING (owner_id > 0);
            "#,
        );
        let table = schema.table(None, "docs").expect("table should exist");

        let missing_audit = Pg2SqliteOptions::default();
        let err = generate_rls_statements(table, &schema, &missing_audit)
            .expect_err("missing audit table should error");
        assert!(err.to_string().contains("RLS audit table name"));
        let err = generate_readonly_rls_statements(table, &schema, &missing_audit)
            .expect_err("missing audit table should error");
        assert!(err.to_string().contains("RLS audit table name"));

        let options = Pg2SqliteOptions::default()
            .with_rls_audit_table_name("rls_audit")
            .with_strict_rls_validation();
        let statements = generate_rls_statements(table, &schema, &options)
            .expect("full RLS statements should build");
        assert!(!statements.is_empty(), "a guarded table emits at least one statement");
        assert!(
            statements
                .iter()
                .any(|stmt| stmt.to_string().contains("CREATE TRIGGER docs_insert_trigger"))
        );

        let readonly = generate_readonly_rls_statements(table, &schema, &options)
            .expect("readonly RLS should build");
        assert!(!readonly.is_empty(), "a read-only table emits at least one statement");
        assert!(!readonly.iter().any(|stmt| stmt.to_string().contains("docs_insert_trigger")));

        let validation = generate_rls_validation_statements(table, &schema, &options, "rls_audit")
            .expect("validation statements should build");
        assert!(!validation.is_empty(), "validation emits at least one statement");
        assert!(
            validation
                .iter()
                .any(|stmt| stmt.to_string().contains("CREATE VIEW docs_rls_violations"))
        );

        let audit_table =
            generate_rls_audit_table("rls_audit").expect("audit table SQL should parse");
        assert!(audit_table.to_string().contains("CREATE TABLE"));

        let create_table_stmt =
            parse_statements("CREATE TABLE docs(id INTEGER PRIMARY KEY)").remove(0);
        let Statement::CreateTable(create_table) = create_table_stmt else {
            panic!("expected create table");
        };
        let renamed = rename_table_for_rls(&create_table, &options, &schema);
        assert!(renamed.name.to_string().ends_with("_rls"));
    }

    #[test]
    fn rls_generation_supports_quoted_identifiers() {
        let schema = schema_from_sql(
            r#"
            CREATE TABLE "Order Items"("doc id" INTEGER PRIMARY KEY, "owner id" INTEGER, "body text" TEXT);
            ALTER TABLE "Order Items" ENABLE ROW LEVEL SECURITY;
            CREATE POLICY order_items_select ON "Order Items" FOR SELECT USING ("owner id" > 0);
            CREATE POLICY order_items_insert ON "Order Items" FOR INSERT WITH CHECK ("owner id" > 0);
            CREATE POLICY order_items_update ON "Order Items" FOR UPDATE USING ("owner id" > 0) WITH CHECK ("owner id" > 0);
            CREATE POLICY order_items_delete ON "Order Items" FOR DELETE USING ("owner id" > 0);
            "#,
        );
        let table = schema
            .table(None, "\"Order Items\"")
            .expect("quoted table should exist (pass the quoted lookup form)");
        let options = Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit");

        let statements = generate_rls_statements(table, &schema, &options)
            .expect("quoted identifiers in RLS SQL should translate");
        assert!(
            statements.iter().any(|stmt| stmt.to_string().contains("CREATE VIEW")),
            "expected generated RLS view"
        );
    }
}
