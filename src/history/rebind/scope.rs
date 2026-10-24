//! The stored relations a select lists, and what a name resolves to in the
//! schema a statement was written for.

#[cfg(not(feature = "std"))]
use alloc::{borrow::ToOwned, string::String, vec, vec::Vec};

use sql_traits::{
    structs::{IdentifierCase, ParserDB, TargetName},
    traits::{ColumnLike, DatabaseLike, TableLike, ViewLike},
};
use sqlparser::ast::{
    Expr, Ident, ObjectName, ObjectNamePart, Select, SelectItem, SetExpr, TableFactor,
};

use super::Planner;
use crate::script::ConnectionError;

/// A stored relation a name resolves to in the schema the statement was
/// written for.
pub(super) enum Relation<'a> {
    Table(&'a <ParserDB as DatabaseLike>::Table),
    View(&'a <ParserDB as DatabaseLike>::View),
    Other,
}

/// One relation a `FROM` clause lists.
pub(super) struct Listed {
    /// The name the select's references qualify it by, ASCII-folded.
    pub(super) qualifier: String,
    /// Whether the qualifier is an explicit alias.
    pub(super) aliased: bool,
    /// Whether the relation is a common table expression or derived table,
    /// whose columns no stored rename reaches.
    pub(super) lexical: bool,
    /// For a view, the ASCII-folded names of the outputs that are bare column
    /// references and so follow a renamed column, as SQLite renames them.
    pub(super) view: Option<Vec<String>>,
    /// The name a replayed reference uses, the alias or the final name.
    pub(super) replay_name: String,
    /// The columns as the epoch names them, each with its replay name.
    pub(super) columns: Option<Vec<(String, String)>>,
}

impl Listed {
    fn lexical(qualifier: &Ident, aliased: bool) -> Self {
        Self {
            qualifier: qualifier.value.to_ascii_lowercase(),
            aliased,
            lexical: true,
            view: None,
            replay_name: qualifier.value.clone(),
            columns: None,
        }
    }
}

impl Planner<'_> {
    /// Records the relations a `FROM` item names.
    pub(super) fn list(
        &self,
        factor: &TableFactor,
        listed: &mut Vec<Listed>,
    ) -> Result<(), ConnectionError> {
        self.list_as(factor, listed, false)
    }

    /// The relations every `FROM` item of `select` names, joins included.
    pub(super) fn list_from(&self, select: &Select) -> Result<Vec<Listed>, ConnectionError> {
        let mut listed = Vec::new();
        for table in &select.from {
            self.list(&table.relation, &mut listed)?;
            for join in &table.joins {
                self.list(&join.relation, &mut listed)?;
            }
        }
        Ok(listed)
    }

    /// Records a written table, which no common table expression can name.
    pub(super) fn list_target(
        &self,
        factor: &TableFactor,
        listed: &mut Vec<Listed>,
    ) -> Result<(), ConnectionError> {
        self.list_as(factor, listed, true)
    }

    fn list_as(
        &self,
        factor: &TableFactor,
        listed: &mut Vec<Listed>,
        target: bool,
    ) -> Result<(), ConnectionError> {
        match factor {
            TableFactor::Table { name, alias, args: None, .. } => {
                let Some(written) = last(name) else { return Ok(()) };
                if !target && name.0.len() == 1 && self.scopes.binding(written).is_some() {
                    let qualifier = alias.as_ref().map_or(written, |alias| &alias.name);
                    listed.push(Listed::lexical(qualifier, alias.is_some()));
                    return Ok(());
                }
                let (temporary, view, columns) = match resolve(self.epoch, name) {
                    Relation::Table(table) => (table.is_temporary(), None, self.columns(table)?),
                    Relation::View(view) => {
                        (view.is_temporary(), Some(following_outputs(view)), None)
                    }
                    Relation::Other => return Ok(()),
                };
                let replay_name = match alias {
                    Some(alias) => alias.name.value.clone(),
                    None if temporary => written.value.clone(),
                    None => self.current_relation(name)?.unwrap_or_else(|| written.value.clone()),
                };
                listed.push(Listed {
                    qualifier: alias
                        .as_ref()
                        .map_or(&written.value, |alias| &alias.name.value)
                        .to_ascii_lowercase(),
                    aliased: alias.is_some(),
                    lexical: false,
                    view,
                    replay_name,
                    columns,
                });
                Ok(())
            }
            TableFactor::NestedJoin { table_with_joins, .. } => {
                self.list_as(&table_with_joins.relation, listed, target)?;
                for join in &table_with_joins.joins {
                    self.list_as(&join.relation, listed, target)?;
                }
                Ok(())
            }
            TableFactor::Derived { alias: Some(alias), .. }
            | TableFactor::Table { alias: Some(alias), args: Some(_), .. } => {
                listed.push(Listed::lexical(&alias.name, true));
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// A stored table's columns as the epoch names them, each with the name
    /// a replay must use.
    pub(super) fn columns(
        &self,
        table: &<ParserDB as DatabaseLike>::Table,
    ) -> Result<Option<Vec<(String, String)>>, ConnectionError> {
        let Ok(columns) = table.columns(self.epoch) else { return Ok(None) };
        let mut names = Vec::new();
        for column in columns {
            let current = if table.is_temporary() || self.replays_home(table.table_name()) {
                column.column_name().to_owned()
            } else {
                self.epoch
                    .column_identity(column)
                    .and_then(|id| self.final_schema.column_by_identity(id))
                    .map(|current| current.column_name().to_owned())
                    .ok_or_else(|| {
                        ConnectionError::UnavailableColumn {
                            relation: table.table_name().to_owned(),
                            column: column.column_name().to_owned(),
                            statement: self.position.index(),
                        }
                    })?
            };
            names.push((column.column_name().to_owned(), current));
        }
        Ok(Some(names))
    }

    /// The innermost listed relation a qualifier names.
    pub(super) fn listed(&self, qualifier: &str) -> Option<&Listed> {
        let qualifier = qualifier.to_ascii_lowercase();
        self.frames
            .iter()
            .rev()
            .flat_map(|frame| frame.iter().rev())
            .find(|entry| entry.qualifier == qualifier)
    }
}

/// The stored relation `name` resolves to, searching `pg_temp` first for an
/// unqualified name as PostgreSQL does.
pub(super) fn resolve<'a>(schema: &'a ParserDB, name: &ObjectName) -> Relation<'a> {
    let idents: Vec<&Ident> = name.0.iter().filter_map(ObjectNamePart::as_ident).collect();
    let Some(last) = idents.last() else { return Relation::Other };
    let target = TargetName::new(&last.value, last.quote_style.is_some());
    let candidates: Vec<TargetName<'_>> = if idents.len() == 1 {
        vec![target.clone().with_schema("pg_temp", false), target]
    } else {
        let schema_part = idents[idents.len() - 2];
        vec![target.with_schema(&schema_part.value, schema_part.quote_style.is_some())]
    };
    for candidate in candidates {
        if let Ok(Some(table)) =
            schema.table_by_target(candidate.clone(), IdentifierCase::AsWritten)
        {
            return Relation::Table(table);
        }
        if let Ok(Some(view)) = schema.view_by_target(candidate, IdentifierCase::AsWritten) {
            return Relation::View(view);
        }
    }
    Relation::Other
}

/// The ASCII-folded output names of `view` that are bare column references.
pub(super) fn following_outputs(view: &<ParserDB as DatabaseLike>::View) -> Vec<String> {
    let SetExpr::Select(select) = view.definition().body.as_ref() else { return Vec::new() };
    select
        .projection
        .iter()
        .filter_map(|item| {
            match item {
                SelectItem::UnnamedExpr(Expr::Identifier(ident)) => {
                    Some(ident.value.to_ascii_lowercase())
                }
                SelectItem::UnnamedExpr(Expr::CompoundIdentifier(parts)) => {
                    parts.last().map(|ident| ident.value.to_ascii_lowercase())
                }
                _ => None,
            }
        })
        .collect()
}

/// The last identifier of `name`, the object it names.
pub(super) fn last(name: &ObjectName) -> Option<&Ident> {
    name.0.iter().filter_map(ObjectNamePart::as_ident).next_back()
}
