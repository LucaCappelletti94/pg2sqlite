//! Planning of queries and the column references in them.

#[cfg(not(feature = "std"))]
use alloc::{borrow::ToOwned, string::String, vec, vec::Vec};

use sql_traits::{
    errors::LookupError,
    structs::{ColumnBinding, ColumnDefinition, ColumnScope, IdentifierCase, ParserDB},
    traits::{ColumnLike, DatabaseLike, TableLike},
};
use sqlparser::{
    ast::{Expr, Ident, InTarget, ObjectName, Query, Select, SelectItem, SetExpr, TableFactor},
    tokenizer::Span,
};

use super::{
    Header, Planner,
    edits::{last_span, quoted},
    scope::last,
};
use crate::{impls::expr_helpers::for_each_child_expr, script::ConnectionError};

/// A column reference resolved in one scope.
pub(super) type Resolved<'s, 'q, 'd> =
    Result<Option<(ColumnDefinition<'s, 'q, 'd, ParserDB>, ColumnBinding<'s>)>, LookupError>;

impl Planner<'_> {
    pub(super) fn root(&mut self, query: &Query, header: Header) -> Result<(), ConnectionError> {
        let Ok(scope) = ColumnScope::from_query(query, self.epoch, IdentifierCase::AsWritten)
        else {
            return Ok(());
        };
        self.query(&scope, query, header)
    }

    pub(super) fn query(
        &mut self,
        scope: &ColumnScope<'_, '_, ParserDB>,
        query: &Query,
        header: Header,
    ) -> Result<(), ConnectionError> {
        self.scopes.enter(query);
        let result = self.query_body(scope, query, header);
        self.scopes.exit();
        result
    }

    fn query_body(
        &mut self,
        scope: &ColumnScope<'_, '_, ParserDB>,
        query: &Query,
        header: Header,
    ) -> Result<(), ConnectionError> {
        if let Some(with) = &query.with {
            for cte in &with.cte_tables {
                self.query(scope, &cte.query, Header::Keep)?;
            }
        }
        self.set_expr(scope, &query.body, header)?;
        if let Some(order_by) = &query.order_by
            && let sqlparser::ast::OrderByKind::Expressions(keys) = &order_by.kind
            && let Some(outputs) = scope.scope_for_query(query)
        {
            let listed = match query.body.as_ref() {
                SetExpr::Select(select) => self.list_from(select)?,
                _ => Vec::new(),
            };
            self.frames.push(listed);
            let mut result = Ok(());
            for key in keys {
                // A key naming an output that follows a renamed column has to
                // follow it too, as SQLite's rename rewrites it.
                if header == Header::Follow
                    && let Expr::Identifier(ident) = &key.expr
                    && let Some(renamed) = self.renamed_output(query, ident)
                {
                    self.edits.rename(ident.span, renamed);
                    continue;
                }
                result = self
                    .expr(
                        scope,
                        &|reference| outputs.resolve_order_by_binding(reference),
                        &key.expr,
                    )
                    .map(|_| ());
                if result.is_err() {
                    break;
                }
            }
            self.frames.pop();
            result?;
        }
        Ok(())
    }

    /// The new name of the unaliased output `key` names, when that output is a
    /// column reference this plan renames.
    pub(super) fn renamed_output(&self, query: &Query, key: &Ident) -> Option<String> {
        let SetExpr::Select(select) = query.body.as_ref() else { return None };
        select.projection.iter().find_map(|item| {
            let SelectItem::UnnamedExpr(expr) = item else { return None };
            let last = match expr {
                Expr::Identifier(ident) => ident,
                Expr::CompoundIdentifier(parts) => parts.last()?,
                _ => return None,
            };
            if !last.value.eq_ignore_ascii_case(&key.value) {
                return None;
            }
            self.edits.renames.get(&last.span).cloned()
        })
    }

    pub(super) fn set_expr(
        &mut self,
        scope: &ColumnScope<'_, '_, ParserDB>,
        body: &SetExpr,
        header: Header,
    ) -> Result<(), ConnectionError> {
        match body {
            SetExpr::Select(select) => self.select(scope, select, header),
            SetExpr::Query(query) => self.query(scope, query, header),
            SetExpr::SetOperation { left, right, .. } => {
                self.set_expr(scope, left, header)?;
                self.set_expr(scope, right, Header::Follow)
            }
            SetExpr::Insert(statement)
            | SetExpr::Update(statement)
            | SetExpr::Delete(statement) => self.statement(statement),
            _ => Ok(()),
        }
    }

    pub(super) fn select(
        &mut self,
        scope: &ColumnScope<'_, '_, ParserDB>,
        select: &Select,
        header: Header,
    ) -> Result<(), ConnectionError> {
        let Some(inner) = scope.scope_for_select(select) else { return Ok(()) };
        let listed = self.list_from(select)?;
        self.frames.push(listed);
        let result = self.select_clauses(scope, &inner, select, header);
        self.frames.pop();
        result
    }

    pub(super) fn select_clauses(
        &mut self,
        scope: &ColumnScope<'_, '_, ParserDB>,
        inner: &sql_traits::structs::ColumnDefinitionScope<'_, '_, '_, ParserDB>,
        select: &Select,
        header: Header,
    ) -> Result<(), ConnectionError> {
        let resolve = |reference: &Expr| inner.resolve_column_binding(reference);
        for item in &select.projection {
            match item {
                SelectItem::UnnamedExpr(expr) => {
                    let renamed = self.expr(scope, &resolve, expr)?;
                    if header == Header::Keep
                        && let Some(previous) = renamed
                        && let Some(span) = last_span(expr)
                    {
                        self.edits.alias(span, quoted(&previous, Span::empty()));
                    }
                }
                SelectItem::ExprWithAlias { expr, .. }
                | SelectItem::ExprWithAliases { expr, .. } => {
                    self.expr(scope, &resolve, expr)?;
                }
                SelectItem::QualifiedWildcard(
                    sqlparser::ast::SelectItemQualifiedWildcardKind::ObjectName(name),
                    _,
                ) => {
                    if let Some(ident) = last(name)
                        && let Some(entry) = self.listed(&ident.value)
                        && !entry.aliased
                        && entry.replay_name != ident.value
                    {
                        let replay_name = entry.replay_name.clone();
                        self.edits.rename(ident.span, replay_name);
                    }
                    if header == Header::Keep {
                        self.expand(select, Some(name));
                    }
                }
                SelectItem::Wildcard(_) => {
                    if header == Header::Keep {
                        self.expand(select, None);
                    }
                }
                SelectItem::QualifiedWildcard(..) => {}
            }
        }
        for table in &select.from {
            self.factor(scope, &resolve, &table.relation)?;
            for join in &table.joins {
                self.factor(scope, &resolve, &join.relation)?;
                if let Some(sqlparser::ast::JoinConstraint::On(expr)) =
                    join_constraint(&join.join_operator)
                {
                    self.expr(scope, &resolve, expr)?;
                }
            }
        }
        if let Some(selection) = &select.selection {
            self.expr(scope, &resolve, selection)?;
        }
        if let sqlparser::ast::GroupByExpr::Expressions(keys, _) = &select.group_by {
            for key in keys {
                self.expr(scope, &|reference| inner.resolve_group_by_binding(reference), key)?;
            }
        }
        if let Some(having) = &select.having {
            self.expr(scope, &resolve, having)?;
        }
        Ok(())
    }

    pub(super) fn factor<'s, 'q, 'd>(
        &mut self,
        scope: &ColumnScope<'_, '_, ParserDB>,
        resolve: &dyn Fn(&Expr) -> Resolved<'s, 'q, 'd>,
        factor: &TableFactor,
    ) -> Result<(), ConnectionError> {
        match factor {
            TableFactor::Derived { subquery, .. } => self.query(scope, subquery, Header::Keep),
            TableFactor::NestedJoin { table_with_joins, .. } => {
                self.factor(scope, resolve, &table_with_joins.relation)?;
                for join in &table_with_joins.joins {
                    self.factor(scope, resolve, &join.relation)?;
                    if let Some(sqlparser::ast::JoinConstraint::On(expr)) =
                        join_constraint(&join.join_operator)
                    {
                        self.expr(scope, resolve, expr)?;
                    }
                }
                Ok(())
            }
            TableFactor::Function { args, .. } => {
                for arg in args {
                    if let sqlparser::ast::FunctionArg::Unnamed(
                        sqlparser::ast::FunctionArgExpr::Expr(expr),
                    ) = arg
                    {
                        self.expr(scope, resolve, expr)?;
                    }
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Plans one expression, returning the previous name of the column it is
    /// when it is a bare reference to a renamed file column.
    pub(super) fn expr<'s, 'q, 'd>(
        &mut self,
        scope: &ColumnScope<'_, '_, ParserDB>,
        resolve: &dyn Fn(&Expr) -> Resolved<'s, 'q, 'd>,
        expr: &Expr,
    ) -> Result<Option<String>, ConnectionError> {
        match expr {
            Expr::Identifier(_) | Expr::CompoundIdentifier(_) => self.column(resolve, expr),
            Expr::Subquery(query) | Expr::Exists { subquery: query, .. } => {
                self.query(scope, query, Header::Follow)?;
                Ok(None)
            }
            Expr::In { expr, target: InTarget::Subquery(subquery), .. } => {
                self.expr(scope, resolve, expr)?;
                self.query(scope, subquery, Header::Follow)?;
                Ok(None)
            }
            _ => {
                let mut result = Ok(());
                for_each_child_expr(expr, &mut |child| {
                    if result.is_ok() {
                        result = self.expr(scope, resolve, child).map(|_| ());
                    }
                });
                result.map(|()| None)
            }
        }
    }

    /// Plans one column reference to a stored table the enclosing selects
    /// list, returning the column's previous name when it was renamed.
    pub(super) fn column<'s, 'q, 'd>(
        &mut self,
        resolve: &dyn Fn(&Expr) -> Resolved<'s, 'q, 'd>,
        expr: &Expr,
    ) -> Result<Option<String>, ConnectionError> {
        let Ok(Some((ColumnDefinition::Base { table, column }, binding))) = resolve(expr) else {
            return Ok(None);
        };
        let parts: Vec<&Ident> = match expr {
            Expr::Identifier(ident) => vec![ident],
            Expr::CompoundIdentifier(parts) => parts.iter().collect(),
            _ => return Ok(None),
        };
        let Some((qualifier, _)) = binding.relation() else { return Ok(None) };
        let Some(entry) = self.listed(qualifier).filter(|entry| !entry.lexical) else {
            return Ok(None);
        };
        let replay_name = entry.replay_name.clone();
        let rename_qualifier =
            !entry.aliased && parts.len() >= 2 && replay_name != parts[parts.len() - 2].value;
        let follows = match &entry.view {
            Some(following) => {
                binding
                    .exposed()
                    .is_some_and(|(exposed, _)| following.contains(&exposed.to_ascii_lowercase()))
            }
            None => true,
        };
        let keeps_columns = !follows || table.is_temporary();
        let through_view = entry.view.is_some();
        if rename_qualifier {
            self.edits.rename(parts[parts.len() - 2].span, replay_name.clone());
        }
        if self.replays_home(table.table_name()) {
            return Ok(None);
        }
        if keeps_columns {
            return Ok(None);
        }
        let Some(written) = parts.last() else { return Ok(None) };
        let current = self
            .epoch
            .column_identity(column)
            .and_then(|id| self.final_schema.column_by_identity(id))
            .ok_or_else(|| {
                ConnectionError::UnavailableColumn {
                    relation: table.table_name().to_owned(),
                    column: column.column_name().to_owned(),
                    statement: self.position.index(),
                }
            })?;
        if parts.len() == 1 && !through_view {
            self.edits.qualify(written.span, quoted(&replay_name, Span::empty()));
        }
        if current.column_name() == column.column_name() {
            return Ok(None);
        }
        self.edits.rename(written.span, current.column_name().to_owned());
        Ok(Some(column.column_name().to_owned()))
    }

    /// Spells out a wildcard whose columns a replay would name differently,
    /// so a table created from the query keeps its column names. A wildcard
    /// over a relation without enumerable columns, or over an outer join that
    /// merges columns, is left as written.
    pub(super) fn expand(&mut self, select: &Select, qualifier: Option<&ObjectName>) {
        let Some(frame) = self.frames.last() else { return };
        let mut items = Vec::new();
        let mut renamed = false;
        let mut seen: Vec<String> = Vec::new();
        let wanted = qualifier.and_then(last).map(|ident| ident.value.to_ascii_lowercase());
        let mut index = 0;
        for table in &select.from {
            let mut shared_columns = vec![Vec::new()];
            for join in &table.joins {
                match merge(&join.join_operator) {
                    Some(Merge::None) => shared_columns.push(Vec::new()),
                    Some(Merge::Using(columns)) => shared_columns.push(columns),
                    Some(Merge::Natural) => shared_columns.push(vec![String::from("*")]),
                    None => return,
                }
            }
            for shared in shared_columns {
                let Some(entry) = frame.get(index) else { return };
                index += 1;
                let Some(columns) = &entry.columns else { return };
                if wanted.as_ref().is_some_and(|wanted| *wanted != entry.qualifier) {
                    continue;
                }
                let natural = shared.first().is_some_and(|name| name == "*");
                for (written, current) in columns {
                    let key = written.to_ascii_lowercase();
                    let merged = wanted.is_none()
                        && (shared.contains(&key) || (natural && seen.contains(&key)));
                    if merged {
                        continue;
                    }
                    seen.push(key);
                    renamed |= current != written;
                    items.push(SelectItem::ExprWithAlias {
                        expr: Expr::CompoundIdentifier(vec![
                            quoted(&entry.replay_name, Span::empty()),
                            quoted(current, Span::empty()),
                        ]),
                        alias: quoted(written, Span::empty()),
                    });
                }
            }
        }
        if renamed && qualifier.is_none() {
            self.edits.expansions.insert(select.select_token.0.span, items);
        }
    }
}

/// How a join combines its two sides' columns.
enum Merge {
    None,
    Using(Vec<String>),
    Natural,
}

/// The column merge an inner join performs, or `None` for a join whose
/// merged columns a wildcard expansion cannot spell out.
fn merge(operator: &sqlparser::ast::JoinOperator) -> Option<Merge> {
    use sqlparser::ast::{JoinConstraint, JoinOperator};
    let (inner, constraint) = match operator {
        JoinOperator::Join(constraint) | JoinOperator::Inner(constraint) => (true, constraint),
        JoinOperator::Left(constraint)
        | JoinOperator::LeftOuter(constraint)
        | JoinOperator::Right(constraint)
        | JoinOperator::RightOuter(constraint)
        | JoinOperator::FullOuter(constraint) => (false, constraint),
        JoinOperator::CrossJoin(_) => return Some(Merge::None),
        _ => return None,
    };
    match constraint {
        JoinConstraint::On(_) | JoinConstraint::None => Some(Merge::None),
        JoinConstraint::Using(columns) if inner => {
            Some(Merge::Using(
                columns
                    .iter()
                    .filter_map(|column| last(column).map(|ident| ident.value.to_ascii_lowercase()))
                    .collect(),
            ))
        }
        JoinConstraint::Natural if inner => Some(Merge::Natural),
        _ => None,
    }
}

fn join_constraint(
    operator: &sqlparser::ast::JoinOperator,
) -> Option<&sqlparser::ast::JoinConstraint> {
    use sqlparser::ast::JoinOperator;
    match operator {
        JoinOperator::Join(constraint)
        | JoinOperator::Inner(constraint)
        | JoinOperator::Left(constraint)
        | JoinOperator::LeftOuter(constraint)
        | JoinOperator::Right(constraint)
        | JoinOperator::RightOuter(constraint)
        | JoinOperator::FullOuter(constraint) => Some(constraint),
        _ => None,
    }
}
