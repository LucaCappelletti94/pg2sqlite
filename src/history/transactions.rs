//! Which source statements survive the batch's transaction control.
//!
//! The outline needs no catalog. A statement is undone by the rollback that
//! covers it or by a transaction still open at the end of the batch, and the
//! statements that survive are exactly the ones the batch commits.

#[cfg(not(feature = "std"))]
use alloc::{vec, vec::Vec};

use sqlparser::ast::{Ident, Statement};

use super::StatementId;
use crate::impls::transaction_blocks::Disposition;

/// The explicit transaction a statement ran inside.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TransactionId(usize);

/// What the batch's transaction control leaves of one statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// The statement's effects persist.
    Committed,
    /// A rollback, or a transaction left open, discards the effects.
    Undone,
    /// A transaction control statement, which has no effects of its own.
    Control,
}

/// Per-statement outcomes and enclosing transactions.
pub(crate) struct Outline {
    outcomes: Vec<Outcome>,
    transactions: Vec<Option<TransactionId>>,
}

struct Frame {
    name: Option<Ident>,
    start: usize,
}

impl Outline {
    /// Reads the outline of `statements`, which `dispositions` has already
    /// validated for PostgreSQL's transaction nesting rules.
    pub(crate) fn new(statements: &[Statement], dispositions: &[Disposition]) -> Self {
        let mut outcomes = vec![Outcome::Committed; statements.len()];
        let mut transactions = vec![None; statements.len()];
        let mut frames: Vec<Frame> = Vec::new();
        let mut pending: Vec<usize> = Vec::new();
        let mut open: Option<TransactionId> = None;
        let mut next = 0;

        for (index, statement) in statements.iter().enumerate() {
            if matches!(dispositions[index], Disposition::DropNoOp { .. }) {
                outcomes[index] = Outcome::Control;
                continue;
            }
            match statement {
                Statement::StartTransaction { .. } => {
                    outcomes[index] = Outcome::Control;
                    open = Some(TransactionId(next));
                    next += 1;
                    frames.push(Frame { name: None, start: 0 });
                }
                Statement::Savepoint { name } => {
                    outcomes[index] = Outcome::Control;
                    frames.push(Frame { name: Some(name.clone()), start: pending.len() });
                }
                Statement::ReleaseSavepoint { name } => {
                    outcomes[index] = Outcome::Control;
                    if let Some(position) = innermost(&frames, name) {
                        frames.truncate(position);
                    }
                }
                Statement::Rollback { savepoint: Some(name), .. } => {
                    outcomes[index] = Outcome::Control;
                    if let Some(position) = innermost(&frames, name) {
                        let start = frames[position].start;
                        for undone in pending.drain(start..) {
                            outcomes[undone] = Outcome::Undone;
                        }
                        frames.truncate(position + 1);
                    }
                }
                Statement::Commit { chain, .. } | Statement::Rollback { chain, .. } => {
                    outcomes[index] = Outcome::Control;
                    let outcome = if matches!(statement, Statement::Commit { .. }) {
                        Outcome::Committed
                    } else {
                        Outcome::Undone
                    };
                    for finished in pending.drain(..) {
                        outcomes[finished] = outcome;
                    }
                    frames.clear();
                    open = None;
                    if *chain {
                        open = Some(TransactionId(next));
                        next += 1;
                        frames.push(Frame { name: None, start: 0 });
                    }
                }
                _ => {
                    if open.is_some() {
                        pending.push(index);
                        transactions[index] = open;
                    }
                }
            }
        }
        for unfinished in pending {
            outcomes[unfinished] = Outcome::Undone;
        }
        Self { outcomes, transactions }
    }

    /// What the batch leaves of `statement`.
    pub(crate) fn outcome(&self, statement: StatementId) -> Outcome {
        self.outcomes[statement.index()]
    }

    /// The explicit transaction `statement` ran inside, if any.
    pub(crate) fn transaction(&self, statement: StatementId) -> Option<TransactionId> {
        self.transactions[statement.index()]
    }
}

/// The most recent frame named `name`, compared as PostgreSQL compares
/// identifiers.
fn innermost(frames: &[Frame], name: &Ident) -> Option<usize> {
    frames.iter().rposition(|frame| frame.name.as_ref().is_some_and(|frame| same_name(frame, name)))
}

fn same_name(left: &Ident, right: &Ident) -> bool {
    match (left.quote_style, right.quote_style) {
        (None, None) => left.value.eq_ignore_ascii_case(&right.value),
        (None, Some(_)) => left.value.to_ascii_lowercase() == right.value,
        (Some(_), None) => left.value == right.value.to_ascii_lowercase(),
        (Some(_), Some(_)) => left.value == right.value,
    }
}
