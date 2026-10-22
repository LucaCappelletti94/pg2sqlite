//! The source batch as a history, and the connection program derived from it.
//!
//! A database file keeps what the batch committed to it, while `TEMP` objects
//! and their rows end with the connection. Reconnecting therefore replays the
//! committed history of the connection objects that survive the batch, and
//! refuses when that history would write the database file again.

mod effects;
mod rebind;
mod transactions;

#[cfg(not(feature = "std"))]
use alloc::{format, string::String, vec, vec::Vec};

use sql_traits::{
    structs::{ParserDB, ParserDBIngestor},
    traits::{DatabaseLike, TableLike, ViewLike},
};
use sqlparser::{ast::Statement, dialect::PostgreSqlDialect, parser::Parser};

use self::{
    effects::{Catalog, Effects},
    transactions::{Outcome, Outline, TransactionId},
};
use crate::{
    errors::{Error, RefusalCategory, TranslationDirection, TranslationRefusal},
    impls::transaction_blocks::Disposition,
    options::TranslationContext,
    pg2sqlite::Pg2Sqlite,
    script::ConnectionError,
    traits::translator::TranslatorWithContext,
};

/// The position of a statement in the source batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct StatementId(usize);

impl StatementId {
    pub(crate) fn index(self) -> usize {
        self.0
    }
}

/// One source statement and its translation.
pub(crate) struct Unit<'a> {
    /// The source statement.
    pub(crate) source: &'a Statement,
    /// The schema the source statement's references bind against.
    pub(crate) binding: &'a ParserDB,
    /// The emitted statements, in order.
    pub(crate) emitted: &'a [Statement],
    /// How PostgreSQL's transaction nesting rules classify the statement.
    pub(crate) disposition: &'a Disposition,
}

/// The statements a connection to an existing database file runs.
///
/// The error says why the connection part cannot be produced, which leaves
/// initialization usable.
pub(crate) fn connection_program(
    units: &[Unit<'_>],
    final_schema: &ParserDB,
    context: &TranslationContext<'_>,
) -> Result<Vec<Statement>, ConnectionError> {
    let sources: Vec<Statement> = units.iter().map(|unit| unit.source.clone()).collect();
    let dispositions: Vec<Disposition> = units.iter().map(|unit| *unit.disposition).collect();
    let outline = Outline::new(&sources, &dispositions);

    let mut catalog = Catalog::default();
    let effects: Vec<Effects> = units
        .iter()
        .enumerate()
        .map(|(index, unit)| {
            match outline.outcome(StatementId(index)) {
                Outcome::Committed => catalog.apply(unit.emitted),
                Outcome::Undone | Outcome::Control => Effects::default(),
            }
        })
        .collect();
    let retained = retention(&catalog, &effects);
    if !retained.iter().any(|kept| *kept) {
        return Ok(Vec::new());
    }
    for (index, kept) in retained.iter().enumerate() {
        if *kept && effects[index].persistent_write {
            return Err(refusal(format!(
                "statement {index} of the batch writes rows a surviving TEMP object needs, and \
                 it also writes a table stored in the database file. Replaying it on a new \
                 connection would write that table a second time. Keep TEMP history free of \
                 writes to stored tables."
            )));
        }
    }
    replay(units, &retained, &outline, final_schema, context)
}

/// Which units the surviving connection objects need.
///
/// A unit is needed when it changes a needed object, and everything it
/// touches or reads becomes needed in turn. A historical object becomes needed
/// through a unit that runs before its drop, so the search repeats until no
/// unit is added, which keeps that drop in the program too.
fn retention(catalog: &Catalog, effects: &[Effects]) -> Vec<bool> {
    let mut needed = vec![false; catalog.len()];
    for id in catalog.alive() {
        needed[id.index()] = true;
    }
    let mut retained = vec![false; effects.len()];
    let mut changed = true;
    while changed {
        changed = false;
        for (index, effect) in effects.iter().enumerate().rev() {
            if retained[index] || !effect.touches.iter().any(|id| needed[id.index()]) {
                continue;
            }
            retained[index] = true;
            changed = true;
            for id in effect.touches.iter().chain(&effect.reads) {
                needed[id.index()] = true;
            }
        }
    }
    retained
}

/// Emits the retained units in source order, grouping the ones a source
/// transaction committed together.
fn replay(
    units: &[Unit<'_>],
    retained: &[bool],
    outline: &Outline,
    final_schema: &ParserDB,
    context: &TranslationContext<'_>,
) -> Result<Vec<Statement>, ConnectionError> {
    let mut schema = ReplaySchema::new(final_schema)?;
    let mut program = Vec::new();
    let mut open: Option<TransactionId> = None;
    for (index, unit) in units.iter().enumerate() {
        if !retained[index] {
            continue;
        }
        let transaction = outline.transaction(StatementId(index));
        if transaction != open {
            if open.is_some() {
                program.push(commit());
            }
            if transaction.is_some() {
                program.push(begin());
            }
            open = transaction;
        }
        let rebound = rebind::rebind(unit.source, StatementId(index), unit.binding, final_schema)?;
        let body = match unit.source {
            Statement::CreateTrigger(create) => {
                rebind::rebind_trigger_body(create, StatementId(index), unit.binding, final_schema)?
            }
            _ => None,
        };
        let rebound = match (rebound, &body) {
            (None, Some(_)) => Some(unit.source.clone()),
            (rebound, _) => rebound,
        };
        let Some(statement) = rebound else {
            program.extend(unit.emitted.iter().cloned());
            schema = schema.apply(unit.source)?;
            continue;
        };
        // A statement ending an epoch was translated against the schema
        // before it, and every other one against the schema after it.
        let before = Pg2Sqlite::schema_ends_epoch(&statement);
        if !before {
            schema = schema.apply(&statement)?;
        }
        let rebound_context;
        let context = match &body {
            Some(body) => {
                rebound_context = context.with_rebound_body(body);
                &rebound_context
            }
            None => context,
        };
        match statement.translate_with_warnings(&schema.snapshot(), context, &mut |_| {}) {
            Ok(statements) => program.extend(statements),
            Err(Error::TranslationRefusal(refusal)) => {
                return Err(ConnectionError::Translation(refusal));
            }
            Err(error) => {
                return Err(refusal(format!(
                    "statement {index} cannot be replayed on a new connection: {error}"
                )));
            }
        }
        if before {
            schema = schema.apply(&statement)?;
        }
    }
    if open.is_some() {
        program.push(commit());
    }
    Ok(program)
}

/// The schema a replayed statement sees, which is the file's final schema
/// plus the connection objects replayed so far.
struct ReplaySchema(ParserDBIngestor);

impl ReplaySchema {
    fn new(final_schema: &ParserDB) -> Result<Self, ConnectionError> {
        let mut drops = Vec::new();
        for view in final_schema.views().filter(|view| view.is_temporary()) {
            drops.push(format!("DROP VIEW IF EXISTS pg_temp.{} CASCADE", quoted(view.view_name())));
        }
        for table in final_schema.tables().filter(|table| table.is_temporary()) {
            drops.push(format!(
                "DROP TABLE IF EXISTS pg_temp.{} CASCADE",
                quoted(table.table_name())
            ));
        }
        let mut schema = Self(final_schema.clone().into_ingestor());
        for drop in drops {
            let statements = Parser::parse_sql(&PostgreSqlDialect {}, &drop)
                .map_err(|error| refusal(format!("{drop} does not parse: {error}")))?;
            for statement in &statements {
                schema = schema.apply(statement)?;
            }
        }
        Ok(schema)
    }

    fn snapshot(&self) -> ParserDB {
        self.0.snapshot()
    }

    fn apply(self, statement: &Statement) -> Result<Self, ConnectionError> {
        if Pg2Sqlite::schema_statement_is_ignored(statement) {
            return Ok(self);
        }
        self.0.apply_statement(statement.clone()).map(Self).map_err(|error| {
            refusal(format!(
                "replaying `{statement}` on a new connection fails against the final schema: {error}"
            ))
        })
    }
}

fn quoted(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn begin() -> Statement {
    Statement::StartTransaction {
        modes: Vec::new(),
        begin: true,
        transaction: None,
        modifier: None,
        statements: Vec::new(),
        exception: None,
        has_end_keyword: false,
    }
}

fn commit() -> Statement {
    Statement::Commit { chain: false, end: false, modifier: None }
}

pub(super) fn refusal(detail: String) -> ConnectionError {
    ConnectionError::Translation(TranslationRefusal::new(
        TranslationDirection::PostgreSqlToSqlite,
        RefusalCategory::UnrepresentableSemantics,
        detail,
    ))
}
