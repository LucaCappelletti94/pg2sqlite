//! Source statements renamed to the final schema's names by identity.
//!
//! A replayed statement runs where file objects already carry their final
//! names. Every reference a statement makes to a file table or column is
//! resolved against the schema the statement was written for, mapped to its
//! sql-traits identity, and renamed to what that identity is called at the end
//! of the batch. Connection objects keep their historical names, because they
//! are replayed in order.
//!
//! Planning only reads the statement, so references are resolved through
//! `ColumnScope`, which borrows it. The edits are keyed by the source span of
//! each identifier and applied afterwards in one mutable pass.
//!
//! `scope` resolves the stored relations a select lists, `query` plans
//! queries and the column references in them, `dml` plans the columns a
//! data-modifying statement names on its target, and `edits` applies the plan.

mod dml;
mod edits;
mod query;
mod scope;

#[cfg(not(feature = "std"))]
use alloc::{
    borrow::ToOwned,
    format,
    string::{String, ToString},
    vec::Vec,
};
use core::ops::ControlFlow;

use sql_traits::{
    structs::ParserDB,
    traits::{DatabaseLike, TableLike, ViewLike},
};
use sqlparser::{
    ast::{BeginEndStatements, Expr, ObjectName, Statement, TableFactor, VisitMut},
    dialect::PostgreSqlDialect,
    parser::Parser,
};

use self::{
    edits::Edits,
    scope::{Listed, Relation, last, resolve},
};
use super::StatementId;
use crate::{
    history::placement::{Files, Homes, home},
    impls::{
        scoped_relations::relation_occurrences,
        translator_impls::{delete::delete_scope_query, update::update_scope_query},
    },
    script::ConnectionError,
    traits::schema::Schema,
};

/// `statement` with every reference to a file object renamed to the name the
/// final schema gives the same identity, or `None` when no name changed.
pub(super) fn rebind(
    statement: &Statement,
    position: StatementId,
    epoch: &ParserDB,
    final_schema: &ParserDB,
    replay_homes: Files,
    homes: &Homes,
) -> Result<Option<Statement>, ConnectionError> {
    let mut planner = Planner {
        epoch,
        final_schema,
        position,
        replay_homes,
        homes,
        edits: Edits::default(),
        frames: Vec::new(),
    };
    planner.relations(statement)?;
    planner.statement(statement)?;
    if planner.edits.is_empty() {
        return Ok(None);
    }
    let mut rebound = statement.clone();
    let _: ControlFlow<()> = rebound.visit(&mut planner.edits);
    Ok(Some(rebound))
}

/// The statements of the trigger function `create` executes, renamed like
/// [`rebind`], or `None` when no name changed.
pub(super) fn rebind_trigger_body(
    create: &sqlparser::ast::CreateTrigger,
    position: StatementId,
    epoch: &ParserDB,
    final_schema: &ParserDB,
    replay_homes: Files,
    homes: &Homes,
) -> Result<Option<BeginEndStatements>, ConnectionError> {
    let Some(exec_body) = &create.exec_body else { return Ok(None) };
    let Ok(Some((mut body, _))) = epoch.function_body_with_context(&exec_body.func_desc.name)
    else {
        return Ok(None);
    };
    // The body parser keeps no source positions, and edits are keyed by
    // them, so each statement is parsed again from its text.
    let mut statements = Vec::with_capacity(body.statements.len());
    for statement in &body.statements {
        let text = statement.to_string();
        let mut parsed = Parser::parse_sql(&PostgreSqlDialect {}, &text).map_err(|error| {
            super::refusal(format!(
                "statement {} runs a trigger body statement `{text}` that does not parse again \
                 for replay: {error}",
                position.index()
            ))
        })?;
        if parsed.len() != 1 {
            return Err(super::refusal(format!(
                "statement {} runs a trigger body statement `{text}` that parses again as {} \
                 statements",
                position.index(),
                parsed.len()
            )));
        }
        statements.extend(parsed.pop());
    }
    let mut planner = Planner {
        epoch,
        final_schema,
        position,
        replay_homes,
        homes,
        edits: Edits::default(),
        frames: Vec::new(),
    };
    for statement in &statements {
        planner.relations(statement)?;
        planner.statement(statement)?;
    }
    if planner.edits.is_empty() {
        return Ok(None);
    }
    for statement in &mut statements {
        let _: ControlFlow<()> = statement.visit(&mut planner.edits);
    }
    body.statements = statements;
    Ok(Some(body))
}

/// How a query's output names behave when the columns it reads are renamed.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Header {
    /// The names follow the renamed columns, as SQLite's own rename makes a
    /// view's output names follow them.
    Follow,
    /// The names are fixed, as for a table created from a query or a
    /// relation an enclosing query reads by name.
    Keep,
}

struct Planner<'a> {
    epoch: &'a ParserDB,
    final_schema: &'a ParserDB,
    position: StatementId,
    /// The homes the output replays in source order, whose objects keep their
    /// historical names and so are not rebound.
    replay_homes: Files,
    /// The home of each created object, keyed by its SQLite-folded name.
    homes: &'a Homes,
    edits: Edits,
    /// The stored relations each enclosing select lists, innermost last.
    frames: Vec<Vec<Listed>>,
}

impl Planner<'_> {
    /// Renames every name denoting a stored file relation.
    fn relations(&mut self, statement: &Statement) -> Result<(), ConnectionError> {
        for name in relation_occurrences(statement) {
            let Some(ident) = last(&name) else { continue };
            let Some(current) = self.current_relation(&name)? else { continue };
            if current != ident.value {
                self.edits.rename(ident.span, current);
            }
        }
        Ok(())
    }

    /// The final name the file relation `name` denotes, or `None` for a
    /// connection object, a name the epoch schema cannot resolve, or an object
    /// whose home the output replays in source order.
    fn current_relation(&self, name: &ObjectName) -> Result<Option<String>, ConnectionError> {
        match resolve(self.epoch, name) {
            Relation::Table(table) if !table.is_temporary() => {
                if self.replays_home(table.table_name()) {
                    return Ok(None);
                }
                let current = self
                    .epoch
                    .table_identity(table)
                    .and_then(|id| self.final_schema.table_by_identity(id))
                    .ok_or_else(|| self.dropped_relation(table.table_name()))?;
                Ok(Some(current.table_name().to_owned()))
            }
            Relation::View(view) if !view.is_temporary() => {
                if self.replays_home(view.view_name()) {
                    return Ok(None);
                }
                let current = self
                    .epoch
                    .view_identity(view)
                    .and_then(|id| self.final_schema.view_by_identity(id))
                    .ok_or_else(|| self.dropped_relation(view.view_name()))?;
                Ok(Some(current.view_name().to_owned()))
            }
            _ => Ok(None),
        }
    }

    /// Whether the output replays the home of the created object `relation`.
    fn replays_home(&self, relation: &str) -> bool {
        self.replay_homes.contains_placement(home(self.homes, relation))
    }

    fn dropped_relation(&self, relation: &str) -> ConnectionError {
        ConnectionError::UnavailableRelation {
            relation: relation.to_owned(),
            statement: self.position.index(),
        }
    }

    fn statement(&mut self, statement: &Statement) -> Result<(), ConnectionError> {
        match statement {
            Statement::Query(query) => self.root(query, Header::Follow),
            Statement::CreateView(create) => self.root(&create.query, Header::Follow),
            Statement::CreateTable(create) => {
                match &create.query {
                    Some(query) => self.root(query, Header::Keep),
                    None => Ok(()),
                }
            }
            Statement::Insert(insert) => self.insert(insert),
            Statement::Update(update) => {
                if let TableFactor::Table { name, .. } = &update.table.relation
                    && let Relation::Table(table) = resolve(self.epoch, name)
                    && !table.is_temporary()
                {
                    for assignment in &update.assignments {
                        self.assignment_target(table, &assignment.target)?;
                    }
                }
                let scope_query = update_scope_query(update);
                let mut exprs: Vec<&Expr> =
                    update.assignments.iter().map(|assignment| &assignment.value).collect();
                exprs.extend(update.selection.as_ref());
                self.dml(&scope_query, &exprs)
            }
            Statement::Delete(delete) => {
                let scope_query = delete_scope_query(delete);
                let exprs: Vec<&Expr> = delete.selection.iter().collect();
                self.dml(&scope_query, &exprs)
            }
            _ => Ok(()),
        }
    }
}
