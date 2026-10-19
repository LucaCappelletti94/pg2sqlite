//! Connection-local definitions and data replayed against the stored schema.

mod columns;
mod dml;
mod headers;
mod transactions;

use alloc::{
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};
#[cfg(not(feature = "std"))]
use alloc::{
    format,
    string::{String, ToString},
    vec,
    vec::Vec,
};
use core::ops::ControlFlow;

use columns::{ColumnId, Columns};
use sql_traits::{
    structs::ParserDB,
    traits::{DatabaseLike, TableLike},
};
use sqlparser::ast::{
    AlterTable, AlterTableOperation, ColumnOption, ConditionalStatements, CreateIndex, CreateTable,
    CreateTrigger, CreateView, Delete, Expr, Fetch, ForClause, ForeignKeyConstraint, FormatClause,
    FromTable, Ident, Insert, LimitClause, LockClause, ObjectName, ObjectNamePart, OrderBy,
    OrderByKind, PipeOperator, Query, RenameTableNameKind, Select, SelectItem,
    SelectItemQualifiedWildcardKind, SetExpr, Setting, Statement, TableConstraint, TableFactor,
    TableObject, TableWithJoins, Update, Values, VisitMut, VisitorMut, With,
};

use crate::{
    errors::Error,
    impls::{
        object_name::last_ident, scoped_relations::relation_names,
        translator_impls::rls::resolve_trigger_table_name,
    },
    options::Pg2SqliteOptions,
    script::ConnectionError,
};

/// Derives connection-local replay from a source-ordered bootstrap.
pub(crate) fn connection_program(
    bootstrap: &[Statement],
    initial_schema: Option<&ParserDB>,
    options: &Pg2SqliteOptions,
) -> Result<Vec<Statement>, Error> {
    if !bootstrap.iter().any(|statement| {
        match statement {
            Statement::CreateTable(create) => create.temporary,
            Statement::CreateView(create) => create.temporary,
            Statement::CreateTrigger(create) => create.temporary,
            _ => false,
        }
    }) {
        return Ok(Vec::new());
    }
    let state = walk(bootstrap, initial_schema, options)?;
    let selected = replay_steps(&state, bootstrap);
    let mut out = Vec::new();
    let mut open: Option<usize> = None;
    for (index, statement) in bootstrap.iter().enumerate() {
        let step = &state.steps[index];
        if !selected[index] {
            continue;
        }
        if step.destinations.iter().any(|id| state.identities[*id].storage == Storage::Main) {
            return Err(Error::forward_refusal(format!(
                "statement {index} would repeat a persistent write while restoring temporary data"
            )));
        }
        if let Some(tx) = step.transaction {
            if open != Some(tx) {
                if open.is_some() {
                    out.push(commit_statement());
                }
                out.push(begin_statement());
                open = Some(tx);
            }
        } else if open.take().is_some() {
            out.push(commit_statement());
        }
        let mut rewritten = statement.clone();
        if let ControlFlow::Break(error) = Rewriter::replay(&state, index).rewrite(&mut rewritten) {
            return Err(error.into());
        }
        out.push(rewritten);
    }
    if open.is_some() {
        out.push(commit_statement());
    }
    Ok(out)
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Storage {
    Main,
    Temp,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Table,
    View,
    Trigger,
    Index,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    TempDefinition,
    PersistentDefinition,
    TempDataWrite,
    PersistentDataWrite,
    Other,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WriteEvent {
    Insert,
    InsertOrUpdate,
    Update,
    Delete,
}

impl WriteEvent {
    fn mask(self) -> u8 {
        match self {
            Self::Insert => 1,
            Self::Update => 2,
            Self::Delete => 4,
            Self::InsertOrUpdate => Self::Insert.mask() | Self::Update.mask(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Write {
    target: usize,
    event: WriteEvent,
}

struct TriggerWrites {
    identity: usize,
    statement: usize,
    events: u8,
    writes: Vec<Write>,
    retirements: Vec<usize>,
}

impl TriggerWrites {
    fn active(&self, steps: &[Step]) -> bool {
        !steps[self.statement].undone && self.retirements.iter().all(|index| steps[*index].undone)
    }
}

struct Identity {
    index: usize,
    storage: Storage,
    kind: Kind,
    final_name: String,
    final_name_folded: String,
    columns_known: bool,
    columns: Rc<Columns>,
    attached: Option<usize>,
    bases: Vec<usize>,
    bases_unknown: bool,
    dropped: bool,
}

#[derive(Default)]
struct ScopeSnapshot {
    names: BTreeMap<String, usize>,
    columns: BTreeMap<usize, Rc<Columns>>,
}

struct Step {
    class: Class,
    identity: Option<usize>,
    target: Option<usize>,
    destinations: Vec<usize>,
    dependencies: Vec<usize>,
    transaction: Option<usize>,
    undone: bool,
    names: Option<ScopeSnapshot>,
}

struct WalkState {
    identities: Vec<Identity>,
    steps: Vec<Step>,
    tx_committed: BTreeMap<usize, bool>,
    final_main: BTreeMap<String, usize>,
    final_temp: BTreeMap<String, usize>,
}

#[derive(Default)]
struct Walker {
    identities: Vec<Identity>,
    main: BTreeMap<String, usize>,
    temp: BTreeMap<String, usize>,
    steps: Vec<Step>,
    triggers: Vec<TriggerWrites>,
    tx: Option<usize>,
    next_tx: usize,
    tx_committed: BTreeMap<usize, bool>,
    frames: Vec<transactions::Frame>,
    journal: Vec<transactions::Undo>,
}

impl Walker {
    fn with_schema(schema: Option<&ParserDB>, options: &Pg2SqliteOptions) -> Result<Self, Error> {
        let mut walker = Self::default();
        if let Some(schema) = schema {
            for table in schema.tables() {
                let physical =
                    resolve_trigger_table_name(table.table_name(), table, schema, options)?;
                let storage = if table.temporary { Storage::Temp } else { Storage::Main };
                let id = walker.new_identity(
                    storage,
                    Kind::Table,
                    physical.to_ascii_lowercase(),
                    physical,
                );
                walker.identities[id].columns = Rc::new(Columns::from_names(
                    table.columns.iter().map(|column| column.name.clone()),
                ));
                if walker.identities[id].final_name != table.table_name() {
                    let view = walker.new_identity(
                        storage,
                        Kind::View,
                        table.table_name().to_ascii_lowercase(),
                        table.table_name().to_string(),
                    );
                    walker.identities[view].columns = Rc::clone(&walker.identities[id].columns);
                    walker.identities[view].bases.push(id);
                }
            }
        }
        Ok(walker)
    }

    fn owner(&self, folded: &str) -> Option<(Storage, usize)> {
        self.temp
            .get(folded)
            .copied()
            .map(|id| (Storage::Temp, id))
            .or_else(|| self.main.get(folded).copied().map(|id| (Storage::Main, id)))
    }

    fn columns_for_name(&self, name: &ObjectName) -> Option<Rc<Columns>> {
        let (_, id) = self.owner(&fold(last_ident(name)?))?;
        self.identities[id].columns_known.then(|| Rc::clone(&self.identities[id].columns))
    }

    fn snapshot<V: sqlparser::ast::Visit>(
        &self,
        node: &V,
        own: Option<usize>,
        target: Option<usize>,
    ) -> ScopeSnapshot {
        let mut snapshot = ScopeSnapshot::default();
        for name in relation_names(node) {
            if let Some((_, id)) = self.owner(&name) {
                snapshot.names.insert(name, id);
                if self.identities[id].columns_known {
                    snapshot.columns.insert(id, Rc::clone(&self.identities[id].columns));
                }
            }
        }
        for id in own.into_iter().chain(target) {
            let identity = &self.identities[id];
            if !snapshot.names.contains_key(&identity.final_name_folded) {
                snapshot.names.insert(identity.final_name_folded.clone(), id);
            }
            if identity.columns_known {
                snapshot.columns.entry(id).or_insert_with(|| Rc::clone(&identity.columns));
            }
        }
        snapshot
    }

    fn push(&mut self, class: Class) {
        self.push_step(class, None, None, Vec::new());
    }

    fn push_step(
        &mut self,
        class: Class,
        identity: Option<usize>,
        target: Option<usize>,
        destinations: Vec<usize>,
    ) {
        self.steps.push(Step {
            class,
            identity,
            target,
            destinations,
            dependencies: Vec::new(),
            transaction: self.tx,
            undone: false,
            names: None,
        });
    }

    fn next_identity(&self) -> usize {
        self.identities.len()
    }

    fn occupied(&self, storage: Storage, folded: &str) -> bool {
        match storage {
            Storage::Temp => self.temp.contains_key(folded),
            Storage::Main => self.main.contains_key(folded),
        }
    }

    fn kill(&mut self, id: usize, storage: Storage, folded: &str) {
        self.retire_identity(id);
        self.remove_name(storage, folded);
        for other_id in 0..self.identities.len() {
            let other = &self.identities[other_id];
            if other.attached == Some(id) && !other.dropped {
                let name = other.final_name_folded.clone();
                let storage = other.storage;
                self.retire_identity(other_id);
                self.remove_name(storage, &name);
            }
        }
    }

    fn new_identity(
        &mut self,
        storage: Storage,
        kind: Kind,
        folded: String,
        display: String,
    ) -> usize {
        let id = self.next_identity();
        self.identities.push(Identity {
            index: id,
            storage,
            kind,
            final_name: display,
            final_name_folded: folded.clone(),
            columns_known: kind != Kind::Index && kind != Kind::Trigger,
            columns: Rc::new(Columns::default()),
            attached: None,
            bases: Vec::new(),
            bases_unknown: false,
            dropped: false,
        });
        self.record_creation(id);
        self.insert_name(storage, folded, id);
        id
    }

    fn step(&mut self, index: usize, statement: &Statement) -> Result<(), Error> {
        match statement {
            Statement::StartTransaction { statements: inner, .. } => {
                if !inner.is_empty() {
                    return Err(Error::forward_refusal(format!(
                        "statement {index} opens a transaction with embedded statements, which the \
                         emitted bootstrap does not carry"
                    )));
                }
                if self.tx.is_some() {
                    return Err(Error::forward_refusal(format!(
                        "statement {index} opens a nested transaction"
                    )));
                }
                self.open_transaction();
                self.push(Class::Other);
            }
            Statement::Commit { chain, .. } => {
                let active = self.tx.is_some();
                self.finish_transaction(true);
                if active && *chain {
                    self.open_transaction();
                }
                self.push(Class::Other);
            }
            Statement::Rollback { chain, savepoint } => {
                if let Some(name) = savepoint {
                    self.rollback_to(name, index)?;
                } else {
                    let active = self.tx.is_some();
                    self.finish_transaction(false);
                    if active && *chain {
                        self.open_transaction();
                    }
                }
                self.push(Class::Other);
            }
            Statement::Savepoint { name } => {
                self.savepoint(name);
                self.push(Class::Other);
            }
            Statement::ReleaseSavepoint { name } => {
                self.release(name, index)?;
                self.push(Class::Other);
            }
            Statement::CreateTable(create) => self.create_table(create, index)?,
            Statement::CreateView(create) => self.create_view(create, index)?,
            Statement::CreateTrigger(create) => self.create_trigger(create, index)?,
            Statement::CreateIndex(create) => self.create_index(create, index)?,
            Statement::CreateVirtualTable { name, .. } => self.create_virtual_table(name, index)?,
            Statement::Drop { names, temporary, .. } => self.drop(names, *temporary, index)?,
            Statement::DropTrigger(drop) => {
                self.drop_trigger(&drop.trigger_name, index)?;
            }
            Statement::AlterTable(alter) => self.alter(alter, index)?,
            Statement::Insert(insert) => {
                self.dml(index, insert_target(insert)?, insert_event(insert))?;
            }
            Statement::Update(update) => {
                self.dml(index, update_target(update)?, WriteEvent::Update)?;
            }
            Statement::Delete(delete) => {
                self.dml(index, delete_target(delete)?, WriteEvent::Delete)?;
            }
            Statement::Query(query) => {
                match query_action(query) {
                    Some(action) => self.step(index, action)?,
                    None => self.push(Class::Other),
                }
            }
            _ => self.push(Class::Other),
        }
        Ok(())
    }

    fn create_table(&mut self, create: &CreateTable, index: usize) -> Result<(), Error> {
        let Some(ident) = last_ident(&create.name) else {
            return Err(Error::forward_refusal(format!(
                "statement {index} names a table without an identifier"
            )));
        };
        let folded = fold(ident);
        let display = ident.value.clone();
        let storage = if create.temporary { Storage::Temp } else { Storage::Main };
        if self.occupied(storage, &folded) {
            return Err(Error::forward_refusal(format!(
                "statement {index} re-creates an object named {folded} that the bootstrap still holds \
                 in the same storage"
            )));
        }
        let columns = create.query.as_deref().map_or_else(
            || {
                Some(Rc::new(Columns::from_names(
                    create.columns.iter().map(|column| column.name.clone()),
                )))
            },
            |query| headers::query_columns(query, &|name| self.columns_for_name(name)),
        );
        let id = self.new_identity(storage, Kind::Table, folded, display);
        self.identities[id].columns_known = columns.is_some();
        if let Some(columns) = columns {
            self.identities[id].columns = columns;
        }
        self.push_step(
            match storage {
                Storage::Temp => Class::TempDefinition,
                Storage::Main => Class::PersistentDefinition,
            },
            Some(id),
            None,
            Vec::new(),
        );
        Ok(())
    }

    fn create_view(&mut self, create: &CreateView, index: usize) -> Result<(), Error> {
        let Some(ident) = last_ident(&create.name) else {
            return Err(Error::forward_refusal(format!(
                "statement {index} names a view without an identifier"
            )));
        };
        let folded = fold(ident);
        let display = ident.value.clone();
        let storage = if create.temporary { Storage::Temp } else { Storage::Main };
        if self.occupied(storage, &folded) {
            return Err(Error::forward_refusal(format!(
                "statement {index} re-creates an object named {folded} that the bootstrap still holds \
                 in the same storage"
            )));
        }
        let mut columns =
            headers::query_columns(&create.query, &|name| self.columns_for_name(name));
        if !create.columns.is_empty()
            && let Some(columns) = &mut columns
        {
            Rc::make_mut(columns).rename_prefix(create.columns.iter().map(|column| &column.name));
        }
        let (bases, bases_unknown) = self.view_bases(&create.query);
        let id = self.new_identity(storage, Kind::View, folded, display);
        self.identities[id].columns_known = columns.is_some();
        if let Some(columns) = columns {
            self.identities[id].columns = columns;
        }
        self.identities[id].bases = bases;
        self.identities[id].bases_unknown = bases_unknown;
        self.push_step(
            match storage {
                Storage::Temp => Class::TempDefinition,
                Storage::Main => Class::PersistentDefinition,
            },
            Some(id),
            None,
            Vec::new(),
        );
        Ok(())
    }

    fn view_bases(&self, query: &Query) -> (Vec<usize>, bool) {
        let mut bases = Vec::new();
        let mut unknown = false;
        for name in relation_names(query) {
            match self.owner(&name) {
                Some((_, id)) => {
                    match self.identities[id].kind {
                        Kind::Table => bases.push(id),
                        Kind::View => {
                            let identity = &self.identities[id];
                            bases.extend(identity.bases.iter().copied());
                            unknown |= identity.bases_unknown;
                        }
                        _ => unknown = true,
                    }
                }
                None => unknown = true,
            }
        }
        (bases, unknown)
    }

    fn create_trigger(&mut self, create: &CreateTrigger, index: usize) -> Result<(), Error> {
        let Some(ident) = last_ident(&create.name) else {
            return Err(Error::forward_refusal(format!(
                "statement {index} names a trigger without an identifier"
            )));
        };
        let folded = fold(ident);
        let display = ident.value.clone();
        let Some(on_ident) = last_ident(&create.table_name) else {
            return Err(Error::forward_refusal(format!(
                "statement {index} creates a trigger on a table without an identifier"
            )));
        };
        let on_folded = fold(on_ident);
        let Some((on_storage, on_id)) = self.owner(&on_folded) else {
            return Err(Error::forward_refusal(format!(
                "statement {index} creates a trigger on table {on_folded}, which the bootstrap does \
                 not define where it sits"
            )));
        };
        if !matches!(self.identities[on_id].kind, Kind::Table | Kind::View) {
            return Err(Error::forward_refusal(format!(
                "statement {index} creates a trigger on {on_folded}, which the bootstrap defines as \
                 a non-table"
            )));
        }
        let storage = if create.temporary { Storage::Temp } else { on_storage };
        if self.occupied(storage, &folded) {
            return Err(Error::forward_refusal(format!(
                "statement {index} re-creates an object named {folded} that the bootstrap still holds \
                 in the same storage"
            )));
        }
        let id = self.new_identity(storage, Kind::Trigger, folded, display);
        self.identities[id].attached = Some(on_id);
        let statements = match &create.statements {
            Some(ConditionalStatements::Sequence { statements }) => statements.as_slice(),
            Some(ConditionalStatements::BeginEnd(block)) => block.statements.as_slice(),
            None => &[],
        };
        let mut writes = Vec::new();
        for statement in statements {
            if let Some((name, event)) = statement_write(statement)? {
                let Some((_, target)) = self.owner(&name) else {
                    return Err(Error::forward_refusal(format!(
                        "statement {index} creates a trigger writing to unavailable relation {name}"
                    )));
                };
                writes.push(Write { target, event });
            }
        }
        let events = create.events.iter().fold(0, |events, event| {
            events
                | match event {
                    sqlparser::ast::TriggerEvent::Insert => WriteEvent::Insert.mask(),
                    sqlparser::ast::TriggerEvent::Update(_) => WriteEvent::Update.mask(),
                    sqlparser::ast::TriggerEvent::Delete => WriteEvent::Delete.mask(),
                    sqlparser::ast::TriggerEvent::Truncate => 0,
                }
        });
        self.triggers.push(TriggerWrites {
            identity: id,
            statement: index,
            events,
            writes,
            retirements: Vec::new(),
        });
        self.push_step(
            match storage {
                Storage::Temp => Class::TempDefinition,
                Storage::Main => Class::PersistentDefinition,
            },
            Some(id),
            Some(on_id),
            Vec::new(),
        );
        Ok(())
    }

    fn create_index(&mut self, create: &CreateIndex, index: usize) -> Result<(), Error> {
        let Some(name) = create.name.as_ref().and_then(last_ident) else {
            return Err(Error::forward_refusal(format!(
                "statement {index} creates an index without a name"
            )));
        };
        let folded = fold(name);
        let display = name.value.clone();
        let Some(table_ident) = last_ident(&create.table_name) else {
            return Err(Error::forward_refusal(format!(
                "statement {index} creates an index on a table without an identifier"
            )));
        };
        let table_folded = fold(table_ident);
        let Some((storage, table_id)) = self.owner(&table_folded) else {
            return Err(Error::forward_refusal(format!(
                "statement {index} creates an index on table {table_folded}, which the bootstrap \
                 does not define where it sits"
            )));
        };
        if self.identities[table_id].kind != Kind::Table {
            return Err(Error::forward_refusal(format!(
                "statement {index} creates an index on {table_folded}, which the bootstrap defines \
                 as a non-table"
            )));
        }
        if self.occupied(storage, &folded) {
            return Err(Error::forward_refusal(format!(
                "statement {index} re-creates an object named {folded} that the bootstrap still holds \
                 in the same storage"
            )));
        }
        let id = self.new_identity(storage, Kind::Index, folded, display);
        self.identities[id].attached = Some(table_id);
        self.push_step(
            match storage {
                Storage::Temp => Class::TempDefinition,
                Storage::Main => Class::PersistentDefinition,
            },
            Some(id),
            Some(table_id),
            Vec::new(),
        );
        Ok(())
    }

    fn create_virtual_table(&mut self, name: &ObjectName, index: usize) -> Result<(), Error> {
        let Some(ident) = last_ident(name) else {
            return Err(Error::forward_refusal(format!(
                "statement {index} names a virtual table without an identifier"
            )));
        };
        let folded = fold(ident);
        let display = ident.value.clone();
        if self.occupied(Storage::Main, &folded) {
            return Err(Error::forward_refusal(format!(
                "statement {index} re-creates an object named {folded} that the bootstrap still holds \
                 in the same storage"
            )));
        }
        let id = self.new_identity(Storage::Main, Kind::Table, folded, display);
        self.identities[id].columns_known = false;
        self.push_step(Class::PersistentDefinition, Some(id), None, Vec::new());
        Ok(())
    }

    fn drop(&mut self, names: &[ObjectName], temporary: bool, index: usize) -> Result<(), Error> {
        let mut identity: Option<usize> = None;
        let mut storage: Option<Storage> = None;
        for name in names {
            let Some(ident) = last_ident(name) else {
                return Err(Error::forward_refusal(format!(
                    "statement {index} drops an object without an identifier"
                )));
            };
            let folded = fold(ident);
            let temp_owner = self.temp.get(&folded).copied();
            let owner = if temporary {
                temp_owner.map(|id| (Storage::Temp, id))
            } else {
                self.owner(&folded)
            };
            if let Some((owner_storage, id)) = owner {
                if identity.is_none() {
                    identity = Some(id);
                    storage = Some(owner_storage);
                }
                for trigger in &mut self.triggers {
                    if self.identities[trigger.identity].attached == Some(id) {
                        trigger.retirements.push(index);
                    }
                }
                self.kill(id, owner_storage, &folded);
            }
        }
        self.push_step(
            match storage {
                Some(Storage::Temp) => Class::TempDefinition,
                Some(Storage::Main) => Class::PersistentDefinition,
                None => Class::Other,
            },
            identity,
            None,
            Vec::new(),
        );
        Ok(())
    }

    fn drop_trigger(&mut self, name: &ObjectName, index: usize) -> Result<(), Error> {
        let folded = fold(target_ident(name)?);
        let Some(position) = self.triggers.iter().rposition(|trigger| {
            self.identities[trigger.identity].final_name_folded == folded
                && trigger.active(&self.steps)
        }) else {
            self.push(Class::Other);
            return Ok(());
        };
        let id = self.triggers[position].identity;
        let storage = self.identities[id].storage;
        self.triggers[position].retirements.push(index);
        self.kill(id, storage, &folded);
        self.push_step(
            match storage {
                Storage::Temp => Class::TempDefinition,
                Storage::Main => Class::PersistentDefinition,
            },
            Some(id),
            None,
            Vec::new(),
        );
        Ok(())
    }

    fn alter(&mut self, alter: &AlterTable, index: usize) -> Result<(), Error> {
        let Some(name) = last_ident(&alter.name) else {
            return Err(Error::forward_refusal(format!(
                "statement {index} alters a table without an identifier"
            )));
        };
        let folded = fold(name);
        let Some((storage, id)) = self.owner(&folded) else {
            return Err(Error::forward_refusal(format!(
                "statement {index} alters unavailable table `{folded}`"
            )));
        };
        for operation in &alter.operations {
            match operation {
                AlterTableOperation::RenameTable { table_name } => {
                    let new_name = match table_name {
                        RenameTableNameKind::As(name) | RenameTableNameKind::To(name) => name,
                    };
                    let Some(new_ident) = last_ident(new_name) else {
                        return Err(Error::forward_refusal(format!(
                            "statement {index} renames a table to a name without an identifier"
                        )));
                    };
                    let new_folded = fold(new_ident);
                    if self.occupied(storage, &new_folded) {
                        return Err(Error::forward_refusal(format!(
                            "statement {index} renames table {folded} onto {new_folded}, which the \
                             bootstrap already holds in the same storage"
                        )));
                    }
                    let old_folded = self.identities[id].final_name_folded.clone();
                    self.remove_name(storage, &old_folded);
                    self.insert_name(storage, new_folded.clone(), id);
                    self.rename_identity(id, &new_ident.value, new_folded);
                }
                AlterTableOperation::RenameColumn { old_column_name, new_column_name, .. } => {
                    self.rename_column(id, old_column_name, new_column_name);
                }
                AlterTableOperation::AddColumn { column_def, .. } => {
                    self.checkpoint_columns(id);
                    let ident = &mut self.identities[id];
                    Rc::make_mut(&mut ident.columns).add(column_def.name.clone());
                }
                AlterTableOperation::DropColumn { column_names, .. } => {
                    self.checkpoint_columns(id);
                    let ident = &mut self.identities[id];
                    for column in column_names {
                        Rc::make_mut(&mut ident.columns).remove(column);
                    }
                }
                _ => {
                    return Err(Error::forward_refusal(format!(
                        "statement {index} alters table {folded} with an operation the translator \
                         does not emit"
                    )));
                }
            }
        }
        self.push_step(
            match storage {
                Storage::Temp => Class::TempDefinition,
                Storage::Main => Class::PersistentDefinition,
            },
            None,
            Some(id),
            Vec::new(),
        );
        Ok(())
    }

    fn rename_column(&mut self, id: usize, old: &Ident, new: &Ident) {
        self.checkpoint_columns(id);
        Rc::make_mut(&mut self.identities[id].columns).rename(old, new);
    }

    fn dml(&mut self, index: usize, target: &Ident, event: WriteEvent) -> Result<(), Error> {
        let folded = fold(target);
        let display = &target.value;
        let Some((_, id)) = self.owner(&folded) else {
            return Err(Error::forward_refusal(format!(
                "statement {index} writes to {display}, which the bootstrap does not define where \
                 it sits"
            )));
        };
        if !matches!(self.identities[id].kind, Kind::Table | Kind::View) {
            return Err(Error::forward_refusal(format!(
                "statement {index} writes to {display}, which the bootstrap defines as a non-relation"
            )));
        }
        let mut writes = vec![Write { target: id, event }];
        let mut dependencies = Vec::new();
        let mut cursor = 0;
        while cursor < writes.len() {
            let write = writes[cursor];
            cursor += 1;
            for base in &self.identities[write.target].bases {
                if !dependencies.contains(base) {
                    dependencies.push(*base);
                }
            }
            for trigger in &self.triggers {
                let identity = &self.identities[trigger.identity];
                if identity.attached != Some(write.target)
                    || !trigger.active(&self.steps)
                    || trigger.events & write.event.mask() == 0
                {
                    continue;
                }
                if !dependencies.contains(&trigger.identity) {
                    dependencies.push(trigger.identity);
                }
                if let Some(scope) = &self.steps[trigger.statement].names {
                    for dependency in scope.names.values() {
                        let name = &self.identities[*dependency].final_name_folded;
                        if let Some((_, dependency)) = self.owner(name)
                            && !dependencies.contains(&dependency)
                        {
                            dependencies.push(dependency);
                        }
                    }
                }
                for effect in &trigger.writes {
                    let name = &self.identities[effect.target].final_name_folded;
                    let Some((_, target)) = self.owner(name) else {
                        return Err(Error::forward_refusal(format!(
                            "statement {index} invokes a trigger writing to unavailable relation {name}"
                        )));
                    };
                    let effect = Write { target, event: effect.event };
                    if !writes.contains(&effect) {
                        writes.push(effect);
                    }
                }
            }
        }
        let mut destinations = Vec::new();
        for write in writes {
            if self.identities[write.target].kind == Kind::Table
                && !destinations.contains(&write.target)
            {
                destinations.push(write.target);
            }
        }
        let class = if destinations.iter().any(|id| self.identities[*id].storage == Storage::Temp) {
            Class::TempDataWrite
        } else {
            Class::PersistentDataWrite
        };
        self.push_step(class, None, Some(id), destinations);
        self.steps[index].dependencies = dependencies;
        Ok(())
    }
}

fn walk(
    statements: &[Statement],
    initial_schema: Option<&ParserDB>,
    options: &Pg2SqliteOptions,
) -> Result<WalkState, Error> {
    let mut walker = Walker::with_schema(initial_schema, options)?;
    for (index, statement) in statements.iter().enumerate() {
        walker.step(index, statement)?;
        if carries_object_names(statement) {
            let step = &walker.steps[index];
            let snapshot = walker.snapshot(statement, step.identity, step.target);
            walker.steps[index].names = Some(snapshot);
        }
    }
    walker.finish_transaction(false);
    let mut state = WalkState {
        identities: walker.identities,
        steps: walker.steps,
        tx_committed: walker.tx_committed,
        final_main: walker.main,
        final_temp: walker.temp,
    };
    refresh_view_headers(&mut state, statements);
    Ok(state)
}

fn refresh_view_headers(state: &mut WalkState, statements: &[Statement]) {
    let view_count =
        statements.iter().filter(|statement| matches!(statement, Statement::CreateView(_))).count();
    for _ in 0..view_count {
        let mut changed = false;
        for (index, statement) in statements.iter().enumerate() {
            let Statement::CreateView(create) = statement else { continue };
            let Some(id) = state.steps[index].identity else { continue };
            if state.identities[id].dropped && state.identities[id].storage == Storage::Main {
                continue;
            }
            let mut query = create.query.clone();
            if Rewriter::replay(state, index).visit_query(&mut query).is_break() {
                continue;
            }
            let Some(mut columns) = headers::query_columns(&query, &|name| {
                let folded = fold(last_ident(name)?);
                let id = state.final_temp.get(&folded).or_else(|| state.final_main.get(&folded))?;
                state.identities[*id]
                    .columns_known
                    .then(|| Rc::clone(&state.identities[*id].columns))
            }) else {
                continue;
            };
            if !create.columns.is_empty() {
                Rc::make_mut(&mut columns)
                    .rename_prefix(create.columns.iter().map(|column| &column.name));
            }
            let identity = &mut state.identities[id];
            if identity.columns_known
                && identity
                    .columns
                    .visible()
                    .map(|name| &name.value)
                    .eq(columns.visible().map(|name| &name.value))
            {
                continue;
            }
            Rc::make_mut(&mut identity.columns).replace_names(columns.visible().cloned());
            identity.columns_known = true;
            changed = true;
        }
        if !changed {
            break;
        }
    }
}

fn retained(state: &WalkState, step: &Step) -> bool {
    if step.undone {
        return false;
    }
    if let Some(tx) = step.transaction
        && !state.tx_committed.get(&tx).copied().unwrap_or(false)
    {
        return false;
    }
    matches!(step.class, Class::TempDefinition | Class::TempDataWrite)
}

fn replay_steps(state: &WalkState, statements: &[Statement]) -> Vec<bool> {
    let mut needed: BTreeSet<_> = state
        .identities
        .iter()
        .filter(|identity| identity.storage == Storage::Temp && !identity.dropped)
        .map(|identity| identity.index)
        .collect();
    let mut selected = vec![false; state.steps.len()];
    for (index, step) in state.steps.iter().enumerate().rev() {
        if !retained(state, step) {
            continue;
        }
        let required = step
            .identity
            .into_iter()
            .chain(step.target)
            .chain(step.destinations.iter().copied())
            .any(|id| needed.contains(&id));
        let attached =
            step.identity.and_then(|id| state.identities[id].attached).is_some_and(|id| {
                state.identities[id].storage == Storage::Temp && needed.contains(&id)
            });
        if !required && !attached {
            continue;
        }
        selected[index] = true;
        if let Some(scope) = &step.names {
            needed.extend(
                scope
                    .names
                    .values()
                    .copied()
                    .filter(|id| state.identities[*id].storage == Storage::Temp),
            );
        }
        needed.extend(
            step.identity
                .into_iter()
                .chain(step.target)
                .chain(step.destinations.iter().copied())
                .chain(step.dependencies.iter().copied())
                .filter(|id| state.identities[*id].storage == Storage::Temp),
        );
    }
    for (index, statement) in statements.iter().enumerate() {
        let step = &state.steps[index];
        if matches!(statement, Statement::Drop { .. } | Statement::DropTrigger(_))
            && retained(state, step)
            && step.identity.is_some_and(|id| needed.contains(&id))
        {
            selected[index] = true;
        }
    }
    selected
}

fn carries_object_names(statement: &Statement) -> bool {
    matches!(
        statement,
        Statement::CreateTable(_)
            | Statement::CreateView(_)
            | Statement::CreateTrigger(_)
            | Statement::CreateIndex(_)
            | Statement::Insert(_)
            | Statement::Update(_)
            | Statement::Delete(_)
            | Statement::Query(_)
    )
}

fn begin_statement() -> Statement {
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

fn commit_statement() -> Statement {
    Statement::Commit { chain: false, end: false, modifier: None }
}

fn statement_write(statement: &Statement) -> Result<Option<(String, WriteEvent)>, Error> {
    let (target, event) = match statement {
        Statement::Insert(insert) => (insert_target(insert)?, insert_event(insert)),
        Statement::Update(update) => (update_target(update)?, WriteEvent::Update),
        Statement::Delete(delete) => (delete_target(delete)?, WriteEvent::Delete),
        Statement::Query(query) => return query_action(query).map_or(Ok(None), statement_write),
        _ => return Ok(None),
    };
    Ok(Some((fold(target), event)))
}

fn insert_event(insert: &Insert) -> WriteEvent {
    if matches!(
        &insert.on,
        Some(sqlparser::ast::OnInsert::OnConflict(conflict))
            if matches!(&conflict.action, sqlparser::ast::OnConflictAction::DoUpdate(_))
    ) {
        WriteEvent::InsertOrUpdate
    } else {
        WriteEvent::Insert
    }
}

fn insert_target(insert: &Insert) -> Result<&Ident, Error> {
    match &insert.table {
        TableObject::TableName(name) => target_ident(name),
        _ => {
            Err(Error::forward_refusal(
                "an INSERT target that is not a plain table name reached the emitted bootstrap",
            ))
        }
    }
}

fn update_target(update: &Update) -> Result<&Ident, Error> {
    match &update.table.relation {
        TableFactor::Table { name, .. } => target_ident(name),
        _ => {
            Err(Error::forward_refusal(
                "an UPDATE target that is not a plain table name reached the emitted bootstrap",
            ))
        }
    }
}

fn delete_target(delete: &Delete) -> Result<&Ident, Error> {
    let tables = match &delete.from {
        FromTable::WithFromKeyword(tables) | FromTable::WithoutKeyword(tables) => tables,
    };
    let table = tables.first().ok_or_else(|| {
        Error::forward_refusal("a DELETE without a table reached the emitted bootstrap")
    })?;
    match &table.relation {
        TableFactor::Table { name, .. } => target_ident(name),
        _ => {
            Err(Error::forward_refusal(
                "a DELETE target that is not a plain table name reached the emitted bootstrap",
            ))
        }
    }
}

fn target_ident(name: &ObjectName) -> Result<&Ident, Error> {
    last_ident(name).ok_or_else(|| {
        Error::forward_refusal(
            "a data statement target without an identifier reached the emitted bootstrap",
        )
    })
}

enum Owner<'a> {
    Final(&'a Identity),
    AsIs(usize),
}

enum OpaqueColumns {
    Exact(Rc<Columns>),
    Unknown,
}

impl OpaqueColumns {
    fn contains(&self, column: &str) -> bool {
        match self {
            OpaqueColumns::Exact(names) => names.lookup(column).is_some(),
            OpaqueColumns::Unknown => true,
        }
    }
}

enum Entry {
    Table(usize, bool),
    AliasedTable(usize, bool),
    QualifiedTable(usize, bool),
    Opaque(OpaqueColumns),
    Cte(OpaqueColumns),
    CteReference { frame: usize, binding: usize },
}

impl Entry {
    fn table(identity: usize, rewrite: bool, aliased: bool) -> Self {
        if aliased { Self::AliasedTable(identity, rewrite) } else { Self::Table(identity, rewrite) }
    }
}

enum Resolution {
    Table(usize, ColumnId, bool, bool),
    Opaque,
}

type RewriteFlow<T = ()> = ControlFlow<ConnectionError, T>;

struct Frame {
    bindings: Vec<Binding>,
    visible_ctes: Option<usize>,
}

struct Binding {
    name: String,
    entry: Entry,
}

// Scoped rewriting removes query children until the derived visitor finishes
// the query node.
struct SuspendedQuery {
    with: Option<With>,
    body: SetExpr,
    order_by: Option<OrderBy>,
    limit_clause: Option<LimitClause>,
    fetch: Option<Fetch>,
    locks: Vec<LockClause>,
    for_clause: Option<ForClause>,
    settings: Option<Vec<Setting>>,
    format_clause: Option<FormatClause>,
    pipe_operators: Vec<PipeOperator>,
}

impl SuspendedQuery {
    fn take(query: &mut Query) -> Self {
        let Query {
            with,
            body,
            order_by,
            limit_clause,
            fetch,
            locks,
            for_clause,
            settings,
            format_clause,
            pipe_operators,
        } = query;
        Self {
            with: with.take(),
            body: core::mem::replace(
                body.as_mut(),
                SetExpr::Values(Values {
                    explicit_row: false,
                    value_keyword: false,
                    rows: Vec::new(),
                }),
            ),
            order_by: order_by.take(),
            limit_clause: limit_clause.take(),
            fetch: fetch.take(),
            locks: core::mem::take(locks),
            for_clause: for_clause.take(),
            settings: settings.take(),
            format_clause: format_clause.take(),
            pipe_operators: core::mem::take(pipe_operators),
        }
    }

    fn restore(self, query: &mut Query) {
        query.with = self.with;
        *query.body = self.body;
        query.order_by = self.order_by;
        query.limit_clause = self.limit_clause;
        query.fetch = self.fetch;
        query.locks = self.locks;
        query.for_clause = self.for_clause;
        query.settings = self.settings;
        query.format_clause = self.format_clause;
        query.pipe_operators = self.pipe_operators;
    }
}

struct Rewriter<'a> {
    identities: &'a [Identity],
    final_main: &'a BTreeMap<String, usize>,
    final_temp: &'a BTreeMap<String, usize>,
    scope: Option<&'a ScopeSnapshot>,
    defining: Option<usize>,
    index: usize,
    frames: Vec<Frame>,
    suspended_query: Option<SuspendedQuery>,
    preserve_headers: bool,
    preserved_columns: Option<Rc<Columns>>,
    changed: usize,
    schema_changed: bool,
}

impl<'a> Rewriter<'a> {
    fn replay(state: &'a WalkState, index: usize) -> Self {
        let scope = state.steps[index].names.as_ref();
        let schema_changed = scope.is_some_and(|scope| {
            scope
                .columns
                .iter()
                .any(|(id, columns)| !Rc::ptr_eq(columns, &state.identities[*id].columns))
        });
        Self {
            identities: &state.identities,
            final_main: &state.final_main,
            final_temp: &state.final_temp,
            scope,
            defining: state.steps[index].identity,
            index,
            frames: Vec::new(),
            suspended_query: None,
            preserve_headers: false,
            preserved_columns: None,
            changed: 0,
            schema_changed,
        }
    }

    fn ident(&self, id: usize) -> &'a Identity {
        let ids: &'a [Identity] = self.identities;
        &ids[id]
    }

    fn owner(&self, folded: &str) -> Option<Owner<'a>> {
        let id = *self
            .scope
            .and_then(|scope| scope.names.get(folded))
            .or_else(|| self.final_temp.get(folded))
            .or_else(|| self.final_main.get(folded))?;
        let identity = self.ident(id);
        Some(if identity.storage == Storage::Temp && identity.kind == Kind::Table {
            Owner::AsIs(id)
        } else {
            Owner::Final(identity)
        })
    }

    fn source_columns(&self, id: usize) -> Option<&'a Columns> {
        if !self.ident(id).columns_known {
            return None;
        }
        Some(
            self.scope
                .and_then(|scope| scope.columns.get(&id))
                .unwrap_or(&self.ident(id).columns)
                .as_ref(),
        )
    }

    fn final_identity(&self, id: usize) -> &'a Identity {
        let identity = self.ident(id);
        if identity.dropped
            && identity.storage == Storage::Main
            && let Some(replacement) = self.final_main.get(&identity.final_name_folded)
        {
            return self.ident(*replacement);
        }
        identity
    }

    fn rewrite(&mut self, statement: &mut Statement) -> RewriteFlow {
        match statement {
            Statement::CreateTable(create) => self.rewrite_create_table(create)?,
            Statement::CreateView(create) => {
                self.visit_query(&mut create.query)?;
            }
            Statement::CreateTrigger(create) => self.rewrite_create_trigger(create)?,
            Statement::CreateIndex(create) => self.rewrite_create_index(create)?,
            Statement::Insert(insert) => self.rewrite_insert(insert)?,
            Statement::Update(update) => self.rewrite_update(update)?,
            Statement::Delete(delete) => self.rewrite_delete(delete)?,
            Statement::Query(query) => {
                self.visit_query(query)?;
            }
            _ => {}
        }
        ControlFlow::Continue(())
    }

    fn rewrite_create_table(&mut self, create: &mut CreateTable) -> RewriteFlow {
        let Some(defining) = self.defining else {
            return ControlFlow::Continue(());
        };
        let ident = self.ident(defining);
        let mut frame = Frame { bindings: Vec::new(), visible_ctes: None };
        frame.bindings.push(Binding {
            name: ident.final_name_folded.clone(),
            entry: Entry::Table(defining, false),
        });
        self.frames.push(frame);
        for column in &mut create.columns {
            for option in &mut column.options {
                match &mut option.option {
                    ColumnOption::Default(expr)
                    | ColumnOption::Materialized(expr)
                    | ColumnOption::Alias(expr) => {
                        expr.visit(self)?;
                    }
                    ColumnOption::Check(check) => {
                        check.expr.visit(self)?;
                    }
                    ColumnOption::ForeignKey(foreign_key) => {
                        self.rewrite_foreign_key(foreign_key, defining)?;
                    }
                    _ => {}
                }
            }
        }
        for constraint in &mut create.constraints {
            match constraint {
                TableConstraint::Check(check) => {
                    check.expr.visit(self)?;
                }
                TableConstraint::ForeignKey(foreign_key) => {
                    self.rewrite_foreign_key(foreign_key, defining)?;
                }
                _ => {}
            }
        }
        self.pop_frame();
        if let Some(query) = &mut create.query {
            let previous = core::mem::replace(&mut self.preserve_headers, true);
            self.preserved_columns =
                self.scope.and_then(|scope| scope.columns.get(&defining)).map(Rc::clone);
            self.visit_query(query)?;
            self.preserve_headers = previous;
        }
        ControlFlow::Continue(())
    }

    fn rewrite_foreign_key(
        &mut self,
        foreign_key: &mut ForeignKeyConstraint,
        self_table: usize,
    ) -> RewriteFlow {
        if let Some(folded) = last_ident(&foreign_key.foreign_table).map(fold)
            && let Some(owner) = self.owner(&folded)
            && let Owner::Final(ident) = owner
        {
            rename_name(&mut foreign_key.foreign_table, &ident.final_name);
            for column in &mut foreign_key.referred_columns {
                self.rewrite_named_column(ident.index, column)?;
            }
        }
        for column in &mut foreign_key.columns {
            self.rewrite_named_column(self_table, column)?;
        }
        ControlFlow::Continue(())
    }

    fn rewrite_create_trigger(&mut self, create: &mut CreateTrigger) -> RewriteFlow {
        let Some(defining) = self.defining else {
            return ControlFlow::Continue(());
        };
        let Some(on) = self.ident(defining).attached else {
            return ControlFlow::Continue(());
        };
        let mut on_rewrite = false;
        if let Some(folded) = last_ident(&create.table_name).map(fold)
            && let Some(Owner::Final(ident)) = self.owner(&folded)
        {
            rename_name(&mut create.table_name, &ident.final_name);
            on_rewrite = true;
        }
        if let Some(referenced) = &mut create.referenced_table_name
            && let Some(folded) = last_ident(referenced).map(fold)
            && let Some(Owner::Final(ident)) = self.owner(&folded)
        {
            rename_name(referenced, &ident.final_name);
        }
        let mut frame = Frame { bindings: Vec::new(), visible_ctes: None };
        frame.bindings.push(Binding {
            name: String::from("old"),
            entry: Entry::AliasedTable(on, on_rewrite),
        });
        frame.bindings.push(Binding {
            name: String::from("new"),
            entry: Entry::AliasedTable(on, on_rewrite),
        });
        self.frames.push(frame);
        if let Some(condition) = &mut create.condition {
            condition.visit(self)?;
        }
        match &mut create.statements {
            Some(ConditionalStatements::Sequence { statements }) => {
                for statement in statements {
                    self.rewrite_body_statement(statement)?;
                }
            }
            Some(ConditionalStatements::BeginEnd(block)) => {
                for statement in &mut block.statements {
                    self.rewrite_body_statement(statement)?;
                }
            }
            None => {}
        }
        self.pop_frame();
        ControlFlow::Continue(())
    }

    fn rewrite_body_statement(&mut self, statement: &mut Statement) -> RewriteFlow {
        match statement {
            Statement::Insert(insert) => self.rewrite_insert(insert)?,
            Statement::Update(update) => self.rewrite_update(update)?,
            Statement::Delete(delete) => self.rewrite_delete(delete)?,
            Statement::Query(query) => {
                self.visit_query(query)?;
            }
            _ => {}
        }
        ControlFlow::Continue(())
    }

    fn rewrite_create_index(&mut self, create: &mut CreateIndex) -> RewriteFlow {
        let Some(defining) = self.defining else {
            return ControlFlow::Continue(());
        };
        let Some(table) = self.ident(defining).attached else {
            return ControlFlow::Continue(());
        };
        let mut rewrite = false;
        if let Some(folded) = last_ident(&create.table_name).map(fold) {
            match self.owner(&folded) {
                Some(Owner::Final(ident)) => {
                    rename_name(&mut create.table_name, &ident.final_name);
                    rewrite = true;
                }
                Some(Owner::AsIs(_)) | None => {}
            }
        }
        let table_ident = self.ident(table);
        let mut frame = Frame { bindings: Vec::new(), visible_ctes: None };
        frame.bindings.push(Binding {
            name: table_ident.final_name_folded.clone(),
            entry: Entry::Table(table, rewrite),
        });
        self.frames.push(frame);
        for column in &mut create.columns {
            column.column.expr.visit(self)?;
        }
        for included in &mut create.include {
            self.rewrite_column_in_scope(included)?;
        }
        if let Some(predicate) = &mut create.predicate {
            predicate.visit(self)?;
        }
        self.pop_frame();
        ControlFlow::Continue(())
    }

    fn visit_query(&mut self, query: &mut Query) -> RewriteFlow {
        let columns = if self.preserve_headers {
            self.preserved_columns.take().or_else(|| self.query_columns(query))
        } else {
            None
        };
        let mut frame = Frame { bindings: Vec::new(), visible_ctes: None };
        if let Some(with) = &query.with {
            frame.bindings = headers::with_columns(with, &|name| self.lookup_columns(name))
                .into_iter()
                .map(|(name, columns)| {
                    Binding {
                        name,
                        entry: Entry::Cte(
                            columns.map_or(OpaqueColumns::Unknown, OpaqueColumns::Exact),
                        ),
                    }
                })
                .collect();
        }
        let query_frame = self.frames.len();
        self.frames.push(frame);
        if let Some(with) = &mut query.with {
            let all = with.cte_tables.len();
            for (index, cte) in with.cte_tables.iter_mut().enumerate() {
                self.frames[query_frame].visible_ctes =
                    Some(if with.recursive { all } else { index });
                self.visit_query(&mut cte.query)?;
            }
            self.frames[query_frame].visible_ctes = None;
        }
        let relation_frame = self.visit_set_expr(&mut query.body, columns.as_deref())?;
        let has_relations = relation_frame.is_some();
        if let Some(frame) = relation_frame {
            self.frames.push(frame);
        }
        let shadow = explicit_output_aliases(query);
        if let Some(order_by) = &mut query.order_by
            && let OrderByKind::Expressions(exprs) = &mut order_by.kind
        {
            for item in exprs {
                self.visit_order_expr(&mut item.expr, shadow.as_ref())?;
            }
        }
        query.limit_clause.visit(self)?;
        query.fetch.visit(self)?;
        if has_relations {
            self.pop_frame();
        }
        self.pop_frame();
        ControlFlow::Continue(())
    }

    fn visit_set_expr(
        &mut self,
        body: &mut SetExpr,
        columns: Option<&Columns>,
    ) -> RewriteFlow<Option<Frame>> {
        let frame = match body {
            SetExpr::Select(select) => Some(self.visit_select(select, columns)?),
            SetExpr::SetOperation { left, right, .. } => {
                let frame = self.visit_set_expr(left, columns)?;
                self.visit_set_expr(right, columns)?;
                frame
            }
            SetExpr::Query(query) => {
                self.visit_query(query)?;
                None
            }
            SetExpr::Values(values) => {
                values.visit(self)?;
                None
            }
            SetExpr::Insert(statement)
            | SetExpr::Update(statement)
            | SetExpr::Delete(statement) => {
                self.rewrite_body_statement(statement)?;
                None
            }
            _ => None,
        };
        ControlFlow::Continue(frame)
    }

    fn visit_select(
        &mut self,
        select: &mut Select,
        columns: Option<&Columns>,
    ) -> RewriteFlow<Frame> {
        let mut frame = Frame { bindings: Vec::new(), visible_ctes: None };
        for table in &mut select.from {
            self.bind_factor_into(&mut table.relation, &mut frame.bindings)?;
            for join in &mut table.joins {
                self.bind_factor_into(&mut join.relation, &mut frame.bindings)?;
            }
        }
        self.frames.push(frame);
        let original = core::mem::take(&mut select.projection);
        let mut projection = Vec::with_capacity(original.len());
        for item in original {
            let qualifier = match &item {
                SelectItem::Wildcard(_) => Some(None),
                SelectItem::QualifiedWildcard(
                    SelectItemQualifiedWildcardKind::ObjectName(name),
                    _,
                ) => Some(Some(name)),
                _ => None,
            };
            if self.preserve_headers
                && let Some(qualifier) = qualifier
                && let Some(expanded) =
                    headers::wildcard_columns(select, qualifier, &|name| self.lookup_columns(name))
            {
                for (qualifier, name) in expanded {
                    let expr = qualifier.map_or_else(
                        || Expr::Identifier(name.clone()),
                        |qualifier| Expr::CompoundIdentifier(vec![qualifier, name.clone()]),
                    );
                    projection.push(SelectItem::UnnamedExpr(expr));
                }
            } else {
                projection.push(item);
            }
        }
        let mut names = columns.map(Columns::visible);
        for item in &mut projection {
            let header = names.as_mut().and_then(Iterator::next);
            let changed = self.changed;
            item.visit(self)?;
            if let SelectItem::QualifiedWildcard(
                SelectItemQualifiedWildcardKind::ObjectName(name),
                _,
            ) = item
                && let Some(qualifier) = last_ident(name).map(fold)
                && let Some(Entry::Table(table, true)) = self.qualified_entry(&qualifier)
            {
                rename_name(name, &self.final_identity(*table).final_name);
            }
            if self.changed != changed
                && let Some(header) = header
                && let SelectItem::UnnamedExpr(expr) = item
            {
                let expr = core::mem::replace(expr, Expr::Identifier(header.clone()));
                *item = SelectItem::ExprWithAlias { expr, alias: header.clone() };
            }
        }
        select.visit(self)?;
        select.projection = projection;
        ControlFlow::Continue(self.frames.pop().expect("the query relation frame is present"))
    }

    fn visit_order_expr(
        &mut self,
        expr: &mut Expr,
        shadow: Option<&BTreeSet<String>>,
    ) -> RewriteFlow {
        if let Expr::Identifier(ident) = expr
            && let Some(names) = shadow
            && names.contains(&fold(ident))
        {
            return ControlFlow::Continue(());
        }
        expr.visit(self)
    }

    fn pop_frame(&mut self) {
        let popped = self.frames.pop();
        debug_assert!(popped.is_some(), "frames push and pop unbalanced");
    }

    fn bind_factor_into(
        &mut self,
        factor: &mut TableFactor,
        bindings: &mut Vec<Binding>,
    ) -> RewriteFlow {
        let binding = match factor {
            TableFactor::Table { name, alias, args: None, .. } => {
                let Some(folded) = last_ident(name).map(fold) else {
                    return ControlFlow::Continue(());
                };
                let entry = if name.0.len() == 1
                    && let Some((frame, binding)) = self.cte_binding(&folded)
                {
                    Entry::CteReference { frame, binding }
                } else {
                    match self.owner(&folded) {
                        Some(Owner::Final(ident)) => {
                            Entry::table(ident.index, true, alias.is_some())
                        }
                        Some(Owner::AsIs(id)) => Entry::table(id, false, alias.is_some()),
                        None => Entry::Opaque(OpaqueColumns::Unknown),
                    }
                };
                Some(Binding {
                    name: alias.as_ref().map_or(folded, |alias| fold(&alias.name)),
                    entry,
                })
            }
            TableFactor::Derived { alias: Some(alias), subquery, .. } => {
                Some(Binding {
                    name: fold(&alias.name),
                    entry: Entry::Opaque(self.query_output_names(subquery)),
                })
            }
            TableFactor::Table { name, args: Some(_), alias, .. } => {
                Some(Binding {
                    name: alias.as_ref().map_or_else(
                        || last_ident(name).map(fold).unwrap_or_default(),
                        |alias| fold(&alias.name),
                    ),
                    entry: Entry::Opaque(
                        headers::factor_columns(factor, &|name| self.lookup_columns(name))
                            .map_or(OpaqueColumns::Unknown, OpaqueColumns::Exact),
                    ),
                })
            }
            _ => {
                factor_alias(factor)
                    .map(|name| Binding { name, entry: Entry::Opaque(OpaqueColumns::Unknown) })
            }
        };
        if let Some(binding) = binding {
            bindings.push(binding);
        }
        ControlFlow::Continue(())
    }

    fn cte_binding(&self, name: &str) -> Option<(usize, usize)> {
        for (frame_index, frame) in self.frames.iter().enumerate().rev() {
            for (binding_index, binding) in frame
                .bindings
                .iter()
                .take(frame.visible_ctes.unwrap_or(frame.bindings.len()))
                .enumerate()
                .rev()
            {
                if binding.name == name && matches!(binding.entry, Entry::Cte(_)) {
                    return Some((frame_index, binding_index));
                }
            }
        }
        None
    }

    fn relation_columns<'b>(&'b self, entry: &'b Entry) -> Option<&'b OpaqueColumns> {
        match entry {
            Entry::Opaque(columns) => Some(columns),
            Entry::CteReference { frame, binding } => {
                match &self.frames[*frame].bindings[*binding].entry {
                    Entry::Cte(columns) => Some(columns),
                    _ => unreachable!("a lexical relation references its CTE declaration"),
                }
            }
            _ => None,
        }
    }

    fn lookup_columns(&self, name: &ObjectName) -> Option<Rc<Columns>> {
        let folded = fold(last_ident(name)?);
        if name.0.len() == 1
            && let Some((frame, binding)) = self.cte_binding(&folded)
        {
            return match &self.frames[frame].bindings[binding].entry {
                Entry::Cte(OpaqueColumns::Exact(columns)) => Some(Rc::clone(columns)),
                _ => None,
            };
        }
        let id = match self.owner(&folded)? {
            Owner::Final(identity) => identity.index,
            Owner::AsIs(id) => id,
        };
        self.ident(id).columns_known.then(|| {
            Rc::clone(
                self.scope
                    .and_then(|scope| scope.columns.get(&id))
                    .unwrap_or(&self.ident(id).columns),
            )
        })
    }

    fn query_columns(&self, query: &Query) -> Option<Rc<Columns>> {
        headers::query_columns(query, &|name| self.lookup_columns(name))
    }

    fn query_output_names(&self, query: &Query) -> OpaqueColumns {
        self.query_columns(query).map_or(OpaqueColumns::Unknown, OpaqueColumns::Exact)
    }

    fn rewrite_expr(&mut self, expr: &mut Expr) -> RewriteFlow {
        match expr {
            Expr::Identifier(ident) => {
                let folded = fold(ident);
                if let Some(Resolution::Table(table, column, true, _)) =
                    self.find_unqualified(&folded)
                {
                    let target = self.final_column(table, column)?;
                    if self.schema_changed
                        && self.needs_qualification(table, target)
                        && let Some((qualifier, physical)) = self.qualifier(table)
                    {
                        let qualifier = if physical {
                            &self.final_identity(table).final_name
                        } else {
                            qualifier
                        };
                        *expr = Expr::CompoundIdentifier(vec![
                            Ident::with_quote('"', qualifier),
                            target.clone(),
                        ]);
                        self.changed += 1;
                    } else {
                        self.changed += usize::from(ident.value != target.value);
                        rename_ident(ident, &target.value);
                    }
                }
            }
            Expr::CompoundIdentifier(idents) if idents.len() >= 2 => {
                let position = idents.len() - 1;
                let qualifier = fold(&idents[position - 1]);
                let column = fold(&idents[position]);
                if let Some(Resolution::Table(table, column, true, physical)) =
                    self.find_qualified(&qualifier, &column)
                {
                    let target = self.final_column(table, column)?;
                    self.changed += usize::from(idents[position].value != target.value);
                    rename_ident(&mut idents[position], &target.value);
                    if physical {
                        let name = &self.final_identity(table).final_name;
                        self.changed += usize::from(idents[position - 1].value != *name);
                        rename_ident(&mut idents[position - 1], name);
                    }
                }
            }
            _ => {}
        }
        ControlFlow::Continue(())
    }

    fn needs_qualification(&self, object: usize, name: &Ident) -> bool {
        let folded = fold(name);
        for frame in self.frames.iter().rev() {
            let mut matches = 0;
            let mut target = false;
            for binding in &frame.bindings {
                let (visible, owner) = match &binding.entry {
                    Entry::Table(id, rewrite) | Entry::AliasedTable(id, rewrite) => {
                        let columns = if *rewrite {
                            self.final_identity(*id)
                                .columns_known
                                .then_some(self.final_identity(*id).columns.as_ref())
                        } else {
                            self.source_columns(*id)
                        };
                        (columns.is_none_or(|columns| columns.lookup(&folded).is_some()), Some(*id))
                    }
                    entry => {
                        (
                            self.relation_columns(entry)
                                .is_some_and(|columns| columns.contains(&folded)),
                            None,
                        )
                    }
                };
                if visible {
                    matches += 1;
                    target |= owner == Some(object);
                }
            }
            if matches != 0 {
                return matches != 1 || !target;
            }
        }
        false
    }

    fn resolve_entry(&self, entry: &Entry, column: &str) -> Option<Resolution> {
        match entry {
            Entry::Table(table, rewrite)
            | Entry::AliasedTable(table, rewrite)
            | Entry::QualifiedTable(table, rewrite) => {
                let Some(columns) = self.source_columns(*table) else {
                    return Some(Resolution::Opaque);
                };
                columns.lookup(column).map(|column| {
                    Resolution::Table(*table, column, *rewrite, matches!(entry, Entry::Table(_, _)))
                })
            }
            entry
                if self.relation_columns(entry).is_some_and(|columns| columns.contains(column)) =>
            {
                Some(Resolution::Opaque)
            }
            _ => None,
        }
    }

    fn find_unqualified(&self, column: &str) -> Option<Resolution> {
        for frame in self.frames.iter().rev() {
            let mut found = None;
            for binding in &frame.bindings {
                if matches!(binding.entry, Entry::QualifiedTable(_, _)) {
                    continue;
                }
                if let Some(resolved) = self.resolve_entry(&binding.entry, column) {
                    if found.is_some() {
                        return None;
                    }
                    found = Some(resolved);
                }
            }
            if found.is_some() {
                return found;
            }
        }
        None
    }

    fn qualified_entry(&self, qualifier: &str) -> Option<&Entry> {
        self.frames
            .iter()
            .rev()
            .flat_map(|frame| &frame.bindings)
            .find(|binding| binding.name == qualifier && !matches!(binding.entry, Entry::Cte(_)))
            .map(|binding| &binding.entry)
    }

    fn find_qualified(&self, qualifier: &str, column: &str) -> Option<Resolution> {
        self.qualified_entry(qualifier).and_then(|entry| self.resolve_entry(entry, column))
    }

    fn qualifier(&self, object: usize) -> Option<(&str, bool)> {
        self.frames.iter().rev().flat_map(|frame| &frame.bindings).find_map(|binding| {
            match binding.entry {
                Entry::Table(id, _) if id == object => Some((binding.name.as_str(), true)),
                Entry::AliasedTable(id, _) if id == object => Some((binding.name.as_str(), false)),
                _ => None,
            }
        })
    }

    fn final_column(
        &self,
        table: usize,
        source_column: ColumnId,
    ) -> ControlFlow<ConnectionError, &'a Ident> {
        let original = self.ident(table);
        let identity = self.final_identity(table);
        let column = if identity.index == original.index {
            Some(source_column)
        } else {
            let Some(name) = original.columns.get(source_column) else {
                return ControlFlow::Break(ConnectionError::UnavailableRelation {
                    relation: original.final_name.clone(),
                    statement: self.index,
                });
            };
            identity.columns.lookup(&fold(name))
        };
        if let Some(name) = column.and_then(|column| identity.columns.get(column)) {
            return ControlFlow::Continue(name);
        }
        let source_name = self
            .source_columns(table)
            .and_then(|columns| columns.get(source_column))
            .expect("resolved fields have a source name");
        ControlFlow::Break(ConnectionError::UnavailableColumn {
            relation: original.final_name.clone(),
            column: source_name.value.clone(),
            statement: self.index,
        })
    }

    fn rewrite_column_in_scope(&mut self, name: &mut Ident) -> RewriteFlow {
        if let Some(Resolution::Table(table, column, true, _)) = self.find_unqualified(&fold(name))
        {
            self.rewrite_column(table, column, name)?;
        }
        ControlFlow::Continue(())
    }

    fn rewrite_named_column(&mut self, table: usize, name: &mut Ident) -> RewriteFlow {
        if let Some(column) =
            self.source_columns(table).and_then(|columns| columns.lookup(&fold(name)))
        {
            self.rewrite_column(table, column, name)?;
        }
        ControlFlow::Continue(())
    }

    fn rewrite_column(&mut self, table: usize, column: ColumnId, name: &mut Ident) -> RewriteFlow {
        let target = self.final_column(table, column)?;
        self.changed += usize::from(name.value != target.value);
        rename_ident(name, &target.value);
        ControlFlow::Continue(())
    }
}

impl VisitorMut for Rewriter<'_> {
    type Break = ConnectionError;

    fn pre_visit_query(&mut self, query: &mut Query) -> RewriteFlow {
        self.visit_query(query)?;
        self.suspended_query = Some(SuspendedQuery::take(query));
        ControlFlow::Continue(())
    }

    fn post_visit_query(&mut self, query: &mut Query) -> RewriteFlow {
        self.suspended_query
            .take()
            .expect("a query node is suspended before derived traversal")
            .restore(query);
        ControlFlow::Continue(())
    }

    fn pre_visit_expr(&mut self, expr: &mut Expr) -> RewriteFlow {
        self.rewrite_expr(expr)?;
        ControlFlow::Continue(())
    }

    fn pre_visit_table_factor(&mut self, factor: &mut TableFactor) -> RewriteFlow {
        if let TableFactor::Table { name, args: None, .. } = factor
            && let Some(folded) = last_ident(name).map(fold)
            && !(name.0.len() == 1 && self.cte_binding(&folded).is_some())
            && let Some(Owner::Final(identity)) = self.owner(&folded)
        {
            for id in core::iter::once(&identity.index).chain(&identity.bases) {
                let identity = self.final_identity(*id);
                if identity.dropped && identity.storage == Storage::Main {
                    return ControlFlow::Break(ConnectionError::UnavailableRelation {
                        relation: identity.final_name.clone(),
                        statement: self.index,
                    });
                }
            }
            let final_name = &self.final_identity(identity.index).final_name;
            self.changed +=
                usize::from(last_ident(name).is_some_and(|name| name.value != *final_name));
            rename_name(name, final_name);
        }
        ControlFlow::Continue(())
    }
}

fn fold(ident: &Ident) -> String {
    ident.value.to_ascii_lowercase()
}

fn rename_ident(ident: &mut Ident, to: &str) {
    if ident.value != to {
        ident.value = String::from(to);
        ident.quote_style = Some('"');
    }
}

fn rename_name(name: &mut ObjectName, to: &str) {
    if let Some(ident) = last_ident_mut(name) {
        rename_ident(ident, to);
    }
}

fn last_ident_mut(name: &mut ObjectName) -> Option<&mut Ident> {
    match name.0.last_mut() {
        Some(ObjectNamePart::Identifier(ident)) => Some(ident),
        _ => None,
    }
}

fn factor_alias(factor: &TableFactor) -> Option<String> {
    match factor {
        TableFactor::TableFunction { alias, .. }
        | TableFactor::Function { alias, .. }
        | TableFactor::UNNEST { alias, .. } => alias.as_ref().map(|a| fold(&a.name)),
        _ => None,
    }
}

fn iter_factor(table: &TableWithJoins) -> impl Iterator<Item = &TableFactor> {
    core::iter::once(&table.relation).chain(table.joins.iter().map(|join| &join.relation))
}

fn first_select(query: &Query) -> Option<&Select> {
    let mut body = query.body.as_ref();
    while let SetExpr::SetOperation { left, .. } = body {
        body = left.as_ref();
    }
    match body {
        SetExpr::Select(select) => Some(select),
        _ => None,
    }
}

fn flat_output_names(query: &Query) -> Option<Vec<Ident>> {
    let select = first_select(query)?;
    let mut names = Vec::with_capacity(select.projection.len());
    for item in &select.projection {
        match item {
            SelectItem::ExprWithAlias { alias, .. } => names.push(alias.clone()),
            SelectItem::ExprWithAliases { aliases, .. } => names.extend(aliases.iter().cloned()),
            SelectItem::UnnamedExpr(Expr::Identifier(ident)) => names.push(ident.clone()),
            SelectItem::UnnamedExpr(Expr::CompoundIdentifier(idents)) => {
                names.push(idents.last()?.clone());
            }
            SelectItem::UnnamedExpr(expr) => names.push(Ident::new(expr.to_string())),
            SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(_, _) => return None,
        }
    }
    Some(names)
}

fn explicit_output_aliases(query: &Query) -> Option<BTreeSet<String>> {
    let select = first_select(query)?;
    let mut names = BTreeSet::new();
    for item in &select.projection {
        match item {
            SelectItem::ExprWithAlias { alias, .. } => {
                names.insert(fold(alias));
            }
            SelectItem::ExprWithAliases { aliases, .. } => {
                names.extend(aliases.iter().map(fold));
            }
            _ => {}
        }
    }
    Some(names)
}

fn query_action(query: &Query) -> Option<&Statement> {
    let mut body = query.body.as_ref();
    loop {
        match body {
            SetExpr::Query(query) => body = query.body.as_ref(),
            SetExpr::Insert(statement)
            | SetExpr::Update(statement)
            | SetExpr::Delete(statement) => {
                return Some(statement);
            }
            _ => return None,
        }
    }
}
