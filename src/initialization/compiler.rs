//! Fresh-file selection without reordering source statements or transaction
//! controls.

use alloc::{
    collections::{BTreeMap, BTreeSet},
    string::String,
    vec::Vec,
};

use sqlparser::ast::Statement;

use crate::{
    errors::Error,
    impls::connection_program::Analysis,
    initialization::{InitializationError, InitializationPlan, InitializationStep},
    script::FreshFiles,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Lifetime {
    Main,
    Attached,
    Connection,
}

pub(crate) fn compile(
    statements: Vec<Statement>,
    lifetimes: &[Lifetime],
    main_tables: &BTreeSet<String>,
    attached_tables: &BTreeSet<String>,
    attached_configured: bool,
    analysis: &Analysis,
) -> Result<InitializationPlan<Statement>, Error> {
    let mut masks: Vec<u8> = lifetimes
        .iter()
        .map(|lifetime| {
            match lifetime {
                Lifetime::Main => FreshFiles::Main.mask() | FreshFiles::Both.mask(),
                Lifetime::Attached => FreshFiles::Attached.mask() | FreshFiles::Both.mask(),
                Lifetime::Connection => 7,
            }
        })
        .collect();
    preserve_transactions(&statements, &mut masks, 7);
    let mut replacements: [BTreeMap<usize, Statement>; 2] = [BTreeMap::new(), BTreeMap::new()];
    let mut unavailable = [None, None];
    match analysis.rebase_existing(&statements, attached_tables, &masks, FreshFiles::Main) {
        Ok(changed) => replacements[0] = changed,
        Err(Error::TranslationRefusal(refusal)) => {
            unavailable[0] = Some(InitializationError::Unrepresentable(refusal));
        }
        Err(Error::Connection(capability)) => {
            unavailable[0] = Some(InitializationError::Unavailable(capability));
        }
        Err(error) => return Err(error),
    }
    if attached_configured {
        match analysis.rebase_existing(&statements, main_tables, &masks, FreshFiles::Attached) {
            Ok(changed) => replacements[1] = changed,
            Err(Error::TranslationRefusal(refusal)) => {
                unavailable[1] = Some(InitializationError::Unrepresentable(refusal));
            }
            Err(Error::Connection(capability)) => {
                unavailable[1] = Some(InitializationError::Unavailable(capability));
            }
            Err(error) => return Err(error),
        }
    } else {
        unavailable[1] = Some(InitializationError::NoAttachedDatabase);
    }
    let enabled = FreshFiles::Both.mask()
        | if unavailable[0].is_none() { FreshFiles::Main.mask() } else { 0 }
        | if unavailable[1].is_none() { FreshFiles::Attached.mask() } else { 0 };
    for mask in &mut masks {
        *mask &= enabled;
    }
    let mut steps = Vec::with_capacity(statements.len());
    for (index, statement) in statements.into_iter().enumerate() {
        let mut modes = masks[index];
        let main = replacements[0].remove(&index).filter(|_| modes & FreshFiles::Main.mask() != 0);
        let attached =
            replacements[1].remove(&index).filter(|_| modes & FreshFiles::Attached.mask() != 0);
        if main.is_some() {
            modes &= !FreshFiles::Main.mask();
        }
        if attached.is_some() {
            modes &= !FreshFiles::Attached.mask();
        }
        steps.push(InitializationStep { modes, statement });
        match (main, attached) {
            (Some(main), Some(attached)) if main == attached => {
                steps.push(InitializationStep {
                    modes: FreshFiles::Main.mask() | FreshFiles::Attached.mask(),
                    statement: main,
                });
            }
            (main, attached) => {
                if let Some(statement) = main {
                    steps.push(InitializationStep { modes: FreshFiles::Main.mask(), statement });
                }
                if let Some(statement) = attached {
                    steps
                        .push(InitializationStep { modes: FreshFiles::Attached.mask(), statement });
                }
            }
        }
    }
    Ok(InitializationPlan::new(steps, unavailable))
}

fn preserve_transactions(statements: &[Statement], masks: &mut [u8], enabled: u8) {
    for (statement, mask) in statements.iter().zip(masks) {
        if matches!(
            statement,
            Statement::StartTransaction { .. }
                | Statement::Commit { .. }
                | Statement::Rollback { .. }
                | Statement::Savepoint { .. }
                | Statement::ReleaseSavepoint { .. }
        ) {
            *mask = enabled;
        }
    }
}
