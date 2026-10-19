//! Query output names resolved against source relation shapes.

use alloc::{
    collections::BTreeSet,
    rc::Rc,
    string::{String, ToString},
    vec::Vec,
};

use sqlparser::ast::{
    Expr, Ident, JoinConstraint, JoinOperator, ObjectName, Query, Select, SelectItem,
    SelectItemQualifiedWildcardKind, SetExpr, TableFactor, With,
};

use super::{columns::Columns, flat_output_names, fold, iter_factor};

type Lookup<'a> = dyn Fn(&ObjectName) -> Option<Rc<Columns>> + 'a;

pub(super) fn with_columns(with: &With, lookup: &Lookup<'_>) -> Vec<(String, Option<Rc<Columns>>)> {
    let mut ctes: Vec<(String, Option<Rc<Columns>>)> = with
        .cte_tables
        .iter()
        .map(|cte| {
            let columns = flat_output_names(&cte.query).map(|names| {
                let mut columns = Columns::from_names(names);
                columns.rename_prefix(cte.alias.columns.iter().map(|column| &column.name));
                Rc::new(columns)
            });
            (fold(&cte.alias.name), columns)
        })
        .collect();
    for _ in &with.cte_tables {
        let mut changed = false;
        for (index, cte) in with.cte_tables.iter().enumerate() {
            if ctes[index].1.is_some() {
                continue;
            }
            let visible = if with.recursive { ctes.len() } else { index };
            let columns =
                query_columns(&cte.query, &|name| lexical(name, &ctes[..visible], lookup));
            if let Some(mut columns) = columns {
                if !cte.alias.columns.is_empty() {
                    Rc::make_mut(&mut columns)
                        .rename_prefix(cte.alias.columns.iter().map(|column| &column.name));
                }
                ctes[index].1 = Some(columns);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    ctes
}

pub(super) fn query_columns(query: &Query, lookup: &Lookup<'_>) -> Option<Rc<Columns>> {
    let Some(with) = &query.with else { return body_columns(&query.body, lookup) };
    let ctes = with_columns(with, lookup);
    body_columns(&query.body, &|name| lexical(name, &ctes, lookup))
}

fn lexical(
    name: &ObjectName,
    ctes: &[(String, Option<Rc<Columns>>)],
    lookup: &Lookup<'_>,
) -> Option<Rc<Columns>> {
    if name.0.len() == 1
        && let Some(ident) = name.0[0].as_ident()
        && let Some((_, columns)) =
            ctes.iter().rev().find(|(alias, _)| alias.eq_ignore_ascii_case(&ident.value))
    {
        return columns.as_ref().map(Rc::clone);
    }
    lookup(name)
}

pub(super) fn body_columns(body: &SetExpr, lookup: &Lookup<'_>) -> Option<Rc<Columns>> {
    match body {
        SetExpr::Select(select) => {
            let mut names = Vec::new();
            for item in &select.projection {
                match item {
                    SelectItem::ExprWithAlias { alias, .. } => names.push(alias.clone()),
                    SelectItem::ExprWithAliases { aliases, .. } => {
                        names.extend(aliases.iter().cloned());
                    }
                    SelectItem::UnnamedExpr(Expr::Identifier(ident)) => names.push(ident.clone()),
                    SelectItem::UnnamedExpr(Expr::CompoundIdentifier(parts)) => {
                        names.push(parts.last()?.clone());
                    }
                    SelectItem::UnnamedExpr(expr) => names.push(Ident::new(expr.to_string())),
                    SelectItem::Wildcard(_) => {
                        for (_, name) in wildcard_columns(select, None, lookup)? {
                            names.push(name);
                        }
                    }
                    SelectItem::QualifiedWildcard(
                        SelectItemQualifiedWildcardKind::ObjectName(name),
                        _,
                    ) => {
                        for (_, name) in wildcard_columns(select, Some(name), lookup)? {
                            names.push(name);
                        }
                    }
                    SelectItem::QualifiedWildcard(SelectItemQualifiedWildcardKind::Expr(_), _) => {
                        return None;
                    }
                }
            }
            Some(Rc::new(Columns::from_names(names)))
        }
        SetExpr::SetOperation { left, .. } => body_columns(left, lookup),
        SetExpr::Query(query) => query_columns(query, lookup),
        SetExpr::Values(values) => {
            Some(Rc::new(Columns::from_names(
                (1..=values.rows.first()?.content.len())
                    .map(|index| Ident::new(alloc::format!("column{index}"))),
            )))
        }
        _ => None,
    }
}

pub(super) fn wildcard_columns(
    select: &Select,
    qualifier: Option<&ObjectName>,
    lookup: &Lookup<'_>,
) -> Option<Vec<(Option<Ident>, Ident)>> {
    if let Some(qualifier) = qualifier {
        let qualifier = qualifier.0.last()?.as_ident()?;
        let factor =
            select.from.iter().flat_map(iter_factor).find(|factor| {
                factor_name(factor).is_some_and(|name| fold(name) == fold(qualifier))
            })?;
        let columns = factor_columns(factor, lookup)?;
        let origin = factor_name(factor).cloned();
        return Some(columns.visible().map(|name| (origin.clone(), name.clone())).collect());
    }
    let mut entries = Vec::new();
    for table in &select.from {
        let start = entries.len();
        let columns = factor_columns(&table.relation, lookup)?;
        let origin = factor_name(&table.relation).cloned();
        let mut names = BTreeSet::new();
        for column in columns.visible() {
            names.insert(fold(column));
            entries.push((origin.clone(), column.clone()));
        }
        for join in &table.joins {
            let columns = factor_columns(&join.relation, lookup)?;
            let origin = factor_name(&join.relation).cloned();
            let mut suppressed = BTreeSet::new();
            match join_constraint(&join.join_operator) {
                Some(JoinConstraint::Using(columns) | JoinConstraint::UsingAlias(columns, _)) => {
                    for column in columns {
                        suppressed.insert(fold(column.0.last()?.as_ident()?));
                    }
                }
                Some(JoinConstraint::Natural) => {
                    for column in columns.visible() {
                        let name = fold(column);
                        if names.contains(&name) {
                            suppressed.insert(name);
                        }
                    }
                }
                _ => {}
            }
            for (origin, column) in &mut entries[start..] {
                if suppressed.contains(&fold(column)) {
                    *origin = None;
                }
            }
            for column in columns.visible() {
                let name = fold(column);
                if suppressed.contains(&name) {
                    continue;
                }
                names.insert(name);
                entries.push((origin.clone(), column.clone()));
            }
        }
    }
    Some(entries)
}

fn join_constraint(operator: &JoinOperator) -> Option<&JoinConstraint> {
    match operator {
        JoinOperator::Join(constraint)
        | JoinOperator::Inner(constraint)
        | JoinOperator::Left(constraint)
        | JoinOperator::LeftOuter(constraint)
        | JoinOperator::Right(constraint)
        | JoinOperator::RightOuter(constraint)
        | JoinOperator::FullOuter(constraint)
        | JoinOperator::CrossJoin(constraint)
        | JoinOperator::Semi(constraint)
        | JoinOperator::LeftSemi(constraint)
        | JoinOperator::RightSemi(constraint)
        | JoinOperator::Anti(constraint)
        | JoinOperator::LeftAnti(constraint)
        | JoinOperator::RightAnti(constraint)
        | JoinOperator::StraightJoin(constraint)
        | JoinOperator::AsOf { constraint, .. } => Some(constraint),
        _ => None,
    }
}

pub(super) fn factor_columns(factor: &TableFactor, lookup: &Lookup<'_>) -> Option<Rc<Columns>> {
    let (mut columns, alias) = match factor {
        TableFactor::Table { name, alias, args, .. } => {
            let columns = if args.is_some() { function_columns(name)? } else { lookup(name)? };
            (columns, alias)
        }
        TableFactor::Derived { subquery, alias, .. } => (query_columns(subquery, lookup)?, alias),
        _ => return None,
    };
    if let Some(alias) = alias
        && !alias.columns.is_empty()
    {
        Rc::make_mut(&mut columns).rename_prefix(alias.columns.iter().map(|column| &column.name));
    }
    Some(columns)
}

fn factor_name(factor: &TableFactor) -> Option<&Ident> {
    match factor {
        TableFactor::Table { name, alias, .. } => {
            alias.as_ref().map(|alias| &alias.name).or_else(|| name.0.last()?.as_ident())
        }
        TableFactor::Derived { alias: Some(alias), .. } => Some(&alias.name),
        _ => None,
    }
}

pub(super) fn function_columns(name: &ObjectName) -> Option<Rc<Columns>> {
    let name = name.0.last()?.as_ident()?;
    if !name.value.eq_ignore_ascii_case("json_each")
        && !name.value.eq_ignore_ascii_case("json_tree")
    {
        return None;
    }
    let mut columns = Columns::from_names(
        ["key", "value", "type", "atom", "id", "parent", "fullkey", "path"].map(Ident::new),
    );
    columns.add_hidden(Ident::new("json"));
    columns.add_hidden(Ident::new("root"));
    Some(Rc::new(columns))
}
