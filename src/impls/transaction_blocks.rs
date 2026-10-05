//! Where each statement of a batch sits relative to a transaction block.
//!
//! PostgreSQL decides several statements entirely on that question: a second
//! `BEGIN` and an unmatched `COMMIT` are warnings it shrugs off, while a
//! savepoint outside a transaction block and `CREATE INDEX CONCURRENTLY`
//! inside one are errors. SQLite answers the opposite way in every case: it
//! fails the nested `BEGIN` and the unmatched `COMMIT` with `cannot start a
//! transaction within a transaction` and `cannot commit - no transaction is
//! active`, and it accepts both statements PostgreSQL refuses.
//!
//! The nesting is read from the batch, which is the same thing PostgreSQL
//! reads: the script the translation emits has to be applied from autocommit
//! already, because it leads with `PRAGMA foreign_keys = 1` where the schema
//! declares a foreign key and SQLite makes that pragma a no-op inside a
//! transaction, measured on 3.51.1 as staying 0 both inside the transaction
//! and after the commit.

#[cfg(not(feature = "std"))]
#[allow(unused_imports)]
use alloc::{string::ToString, vec, vec::Vec};

use sqlparser::ast::Statement;

use crate::errors::Error;

/// What the batch's transaction nesting says about one statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Disposition {
    /// The statement translates as it stands.
    Translate,
    /// PostgreSQL performs the statement as a no-op here and says so, so the
    /// translation emits nothing and reports the same thing.
    DropNoOp {
        /// Short identifier of the dropped construct, for the warning.
        construct: &'static str,
        /// Why dropping it reproduces the server, for the warning.
        reason: &'static str,
    },
}

/// Reason for dropping a `BEGIN` that opens nothing.
const REASON_BEGIN_ALREADY_OPEN: &str = "a transaction is already open, where PostgreSQL answers `there is already a transaction in \
     progress` and does nothing, so emitting nothing reproduces the server. SQLite fails the \
     statement with `cannot start a transaction within a transaction`.";

/// Reason for dropping a `COMMIT` or `ROLLBACK` that ends nothing.
const REASON_NO_TRANSACTION_OPEN: &str = "no transaction is open, where PostgreSQL answers `there is no transaction in progress` and \
     does nothing, so emitting nothing reproduces the server. SQLite fails the statement with \
     `cannot commit - no transaction is active`, and every statement after it with it.";

/// Reads the transaction nesting of `statements` and answers what to do with
/// each, refusing the ones PostgreSQL refuses in the position they occupy.
///
/// # Errors
///
/// Returns [`Error::TranslationRefusal`] for a savepoint statement outside a
/// transaction block, for `COMMIT`/`ROLLBACK AND CHAIN` outside one, and for
/// `CREATE INDEX CONCURRENTLY` inside one, each of which PostgreSQL answers
/// with an error rather than a warning.
pub(crate) fn analyse(statements: &[Statement]) -> Result<Vec<Disposition>, Error> {
    let mut dispositions = vec![Disposition::Translate; statements.len()];
    let mut open = false;

    for (statement, disposition) in statements.iter().zip(dispositions.iter_mut()) {
        match statement {
            Statement::StartTransaction { .. } => {
                if open {
                    *disposition = Disposition::DropNoOp {
                        construct: "BEGIN",
                        reason: REASON_BEGIN_ALREADY_OPEN,
                    };
                } else {
                    open = true;
                }
            }
            Statement::Commit { chain, .. } => {
                if open {
                    open = *chain;
                } else if *chain {
                    return Err(outside_a_transaction_block("COMMIT AND CHAIN"));
                } else {
                    *disposition = Disposition::DropNoOp {
                        construct: "COMMIT",
                        reason: REASON_NO_TRANSACTION_OPEN,
                    };
                }
            }
            Statement::Rollback { chain, savepoint } => {
                if savepoint.is_some() {
                    if !open {
                        return Err(outside_a_transaction_block("ROLLBACK TO SAVEPOINT"));
                    }
                } else if open {
                    open = *chain;
                } else if *chain {
                    return Err(outside_a_transaction_block("ROLLBACK AND CHAIN"));
                } else {
                    *disposition = Disposition::DropNoOp {
                        construct: "ROLLBACK",
                        reason: REASON_NO_TRANSACTION_OPEN,
                    };
                }
            }
            Statement::Savepoint { .. } if !open => {
                return Err(outside_a_transaction_block("SAVEPOINT"));
            }
            Statement::ReleaseSavepoint { .. } if !open => {
                return Err(outside_a_transaction_block("RELEASE SAVEPOINT"));
            }
            Statement::CreateIndex(create_index) if open && create_index.concurrently => {
                return Err(Error::forward_refusal(
                    "CREATE INDEX CONCURRENTLY sits inside a transaction block, where PostgreSQL \
                     answers `CREATE INDEX CONCURRENTLY cannot run inside a transaction block`. \
                     SQLite builds the index inside the transaction instead, so a ROLLBACK \
                     removes an index the server never started building. Move the statement \
                     outside the transaction block, where the concurrent build is legal and only \
                     the lock it takes differs."
                        .to_string(),
                ));
            }
            _ => {}
        }
    }

    Ok(dispositions)
}

/// Refuses a statement PostgreSQL allows only inside a transaction block.
fn outside_a_transaction_block(construct: &str) -> Error {
    Error::forward_refusal(alloc::format!(
        "{construct} sits outside a transaction block, where PostgreSQL answers `{construct} can \
         only be used in transaction blocks`. SQLite opens an implicit transaction instead, so \
         the statement succeeds and the writes around it commit, which is a script the server \
         would never have run. Open the transaction with BEGIN first."
    ))
}
