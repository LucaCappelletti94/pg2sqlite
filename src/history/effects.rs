//! What each translation unit does to connection-local objects.
//!
//! Effects are read from the emitted SQLite statements, because some
//! connection objects, such as full-text maintenance triggers, exist only in
//! the translator's output. SQLite resolves view bodies and trigger bodies by
//! name when they run, so both are resolved at the statement that reads or
//! fires them.

#[cfg(not(feature = "std"))]
use alloc::{collections::BTreeMap, string::String, vec::Vec};
#[cfg(feature = "std")]
use std::collections::BTreeMap;

use sqlparser::ast::{
    AlterTable, AlterTableOperation, ConditionalStatements, CreateTrigger, FromTable, ObjectName,
    ObjectNamePart, ObjectType, OnConflictAction, OnInsert, RenameTableNameKind, SetExpr,
    Statement, TableFactor, TableObject, TriggerEvent,
};

use crate::impls::scoped_relations::relation_names;

/// One connection-local object, from its creation to its drop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ObjectId(usize);

/// The row events a write or a trigger covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Events {
    insert: bool,
    update: bool,
    delete: bool,
}

impl Events {
    const INSERT: Self = Self { insert: true, update: false, delete: false };
    const UPDATE: Self = Self { insert: false, update: true, delete: false };
    const DELETE: Self = Self { insert: false, update: false, delete: true };
    const UPSERT: Self = Self { insert: true, update: true, delete: false };

    fn overlaps(self, other: Self) -> bool {
        (self.insert && other.insert)
            || (self.update && other.update)
            || (self.delete && other.delete)
    }

    fn of_trigger(events: &[TriggerEvent]) -> Self {
        let mut covered = Self::default();
        for event in events {
            match event {
                TriggerEvent::Insert => covered.insert = true,
                TriggerEvent::Update(_) => covered.update = true,
                TriggerEvent::Delete => covered.delete = true,
                TriggerEvent::Truncate => {}
            }
        }
        covered
    }
}

/// What one translation unit does to connection-local objects.
#[derive(Debug, Default)]
pub(crate) struct Effects {
    /// Objects whose definition or rows the unit changes.
    pub(crate) touches: Vec<ObjectId>,
    /// Objects that must exist when the unit runs.
    pub(crate) reads: Vec<ObjectId>,
    /// Whether the unit, directly or through a trigger, writes a table stored
    /// in the database file.
    pub(crate) persistent_write: bool,
}

impl Effects {
    fn touch(&mut self, id: ObjectId) {
        if !self.touches.contains(&id) {
            self.touches.push(id);
        }
    }

    fn read(&mut self, id: ObjectId) {
        if !self.reads.contains(&id) {
            self.reads.push(id);
        }
    }
}

enum Shape {
    Table,
    View { bases: Vec<String> },
    Trigger { owner: Owner, events: Events, body: Vec<Statement> },
    Index { owner: ObjectId },
}

/// The relation a trigger is attached to.
#[derive(Clone, PartialEq, Eq)]
enum Owner {
    Connection(ObjectId),
    File(String),
}

struct Object {
    shape: Shape,
    alive: bool,
}

/// The connection-local objects as statements create, rename and drop them.
#[derive(Default)]
pub(crate) struct Catalog {
    objects: Vec<Object>,
    relations: BTreeMap<String, ObjectId>,
    triggers: BTreeMap<String, ObjectId>,
    indexes: BTreeMap<String, ObjectId>,
}

impl Catalog {
    /// Records the effects of one unit and applies its definitions.
    pub(crate) fn apply(&mut self, unit: &[Statement]) -> Effects {
        let mut effects = Effects::default();
        for statement in unit {
            self.statement(statement, &mut effects);
        }
        effects
    }

    /// Objects alive after every applied unit.
    pub(crate) fn alive(&self) -> impl Iterator<Item = ObjectId> + '_ {
        self.objects
            .iter()
            .enumerate()
            .filter(|(_, object)| object.alive)
            .map(|(index, _)| ObjectId(index))
    }

    /// The number of objects ever created, an upper bound on every id.
    pub(crate) fn len(&self) -> usize {
        self.objects.len()
    }

    fn create(&mut self, shape: Shape) -> ObjectId {
        let id = ObjectId(self.objects.len());
        self.objects.push(Object { shape, alive: true });
        id
    }

    fn relation(&self, name: &ObjectName) -> Option<ObjectId> {
        let (schema, key) = split(name)?;
        if schema.as_deref().is_some_and(|schema| !schema.eq_ignore_ascii_case("temp")) {
            return None;
        }
        self.relations.get(&key).copied()
    }

    fn statement(&mut self, statement: &Statement, effects: &mut Effects) {
        match statement {
            Statement::CreateTable(create) if is_temporary(create.temporary, &create.name) => {
                if let Some(query) = &create.query {
                    self.reads(&relation_names(query.as_ref()), effects);
                }
                self.create_relation(&create.name, create.if_not_exists, Shape::Table, effects);
            }
            Statement::CreateVirtualTable { name, if_not_exists, .. }
                if is_temporary(false, name) =>
            {
                self.create_relation(name, *if_not_exists, Shape::Table, effects);
            }
            Statement::CreateView(create) if is_temporary(create.temporary, &create.name) => {
                if create.or_replace
                    && let Some((_, key)) = split(&create.name)
                    && let Some(previous) = self.relations.remove(&key)
                {
                    self.retire(previous, effects);
                }
                let bases = relation_names(create.query.as_ref());
                self.create_relation(&create.name, false, Shape::View { bases }, effects);
            }
            Statement::CreateTrigger(create)
                if create.temporary || self.relation(&create.table_name).is_some() =>
            {
                self.create_trigger(create, effects);
            }
            Statement::CreateIndex(create) => {
                let Some(owner) = self.relation(&create.table_name) else { return };
                let Some((_, key)) = create.name.as_ref().and_then(split) else { return };
                let id = self.create(Shape::Index { owner });
                self.indexes.insert(key, id);
                effects.read(owner);
                effects.touch(id);
            }
            Statement::Drop { object_type, names, .. } => self.drop(*object_type, names, effects),
            Statement::DropTrigger(drop) => {
                let Some((_, key)) = split(&drop.trigger_name) else { return };
                if let Some(id) = self.triggers.remove(&key) {
                    self.retire(id, effects);
                }
            }
            Statement::AlterTable(alter) => self.alter(alter, effects),
            Statement::Insert(_)
            | Statement::Update(_)
            | Statement::Delete(_)
            | Statement::Query(_)
            | Statement::Merge(_) => {
                let Some(write) = row_write(statement) else { return };
                self.reads(&relation_names(statement), effects);
                match write {
                    RowWrite::Table(name, events) => {
                        self.write(name, events, effects, &mut Vec::new());
                    }
                    RowWrite::Untracked => effects.persistent_write = true,
                }
            }
            _ => {}
        }
    }

    fn create_relation(
        &mut self,
        name: &ObjectName,
        if_not_exists: bool,
        shape: Shape,
        effects: &mut Effects,
    ) {
        let Some((_, key)) = split(name) else { return };
        if if_not_exists && self.relations.contains_key(&key) {
            return;
        }
        let id = self.create(shape);
        self.relations.insert(key, id);
        effects.touch(id);
    }

    fn drop(&mut self, object_type: ObjectType, names: &[ObjectName], effects: &mut Effects) {
        for name in names {
            let Some((schema, key)) = split(name) else { continue };
            if schema.as_deref().is_some_and(|schema| !schema.eq_ignore_ascii_case("temp")) {
                continue;
            }
            let map = match object_type {
                ObjectType::Table | ObjectType::View => &mut self.relations,
                ObjectType::Index => &mut self.indexes,
                _ => continue,
            };
            if let Some(id) = map.remove(&key) {
                self.retire(id, effects);
            }
        }
    }

    fn alter(&mut self, alter: &AlterTable, effects: &mut Effects) {
        let Some(id) = self.relation(&alter.name) else { return };
        effects.touch(id);
        for operation in &alter.operations {
            if let AlterTableOperation::RenameTable {
                table_name: RenameTableNameKind::As(to) | RenameTableNameKind::To(to),
            } = operation
                && let (Some((_, from)), Some((_, to))) = (split(&alter.name), split(to))
            {
                self.relations.remove(&from);
                self.relations.insert(to, id);
            }
        }
    }

    fn create_trigger(&mut self, create: &CreateTrigger, effects: &mut Effects) {
        let Some((_, key)) = split(&create.name) else { return };
        let owner = match self.relation(&create.table_name) {
            Some(id) => {
                effects.read(id);
                Owner::Connection(id)
            }
            None => {
                match split(&create.table_name) {
                    Some((_, table)) => Owner::File(table),
                    None => return,
                }
            }
        };
        let body = match &create.statements {
            Some(ConditionalStatements::BeginEnd(block)) => block.statements.clone(),
            Some(ConditionalStatements::Sequence { statements }) => statements.clone(),
            None => Vec::new(),
        };
        if create.or_replace
            && let Some(previous) = self.triggers.remove(&key)
        {
            self.retire(previous, effects);
        }
        let id =
            self.create(Shape::Trigger { owner, events: Events::of_trigger(&create.events), body });
        self.triggers.insert(key, id);
        effects.touch(id);
    }

    /// Records a read of each named relation, including the relations a read
    /// view reads when it runs.
    fn reads(&self, names: &[String], effects: &mut Effects) {
        for name in names {
            let Some(&id) = self.relations.get(name) else { continue };
            if effects.reads.contains(&id) {
                continue;
            }
            effects.read(id);
            if let Shape::View { bases } = &self.objects[id.0].shape {
                self.reads(bases, effects);
            }
        }
    }

    /// Records a row write to `name` and every trigger it fires, transitively.
    fn write(
        &self,
        name: &ObjectName,
        events: Events,
        effects: &mut Effects,
        fired: &mut Vec<(ObjectId, Events)>,
    ) {
        let target = self.relation(name);
        match target {
            Some(id) => {
                match &self.objects[id.0].shape {
                    Shape::Table => effects.touch(id),
                    Shape::View { .. } => effects.read(id),
                    Shape::Trigger { .. } | Shape::Index { .. } => {}
                }
            }
            None => effects.persistent_write = true,
        }
        let owner = match target {
            Some(id) => Owner::Connection(id),
            None => {
                match split(name) {
                    Some((_, table)) => Owner::File(table),
                    None => return,
                }
            }
        };
        for (index, object) in self.objects.iter().enumerate() {
            let Shape::Trigger { owner: on, events: covered, body } = &object.shape else {
                continue;
            };
            if !object.alive || *on != owner || !covered.overlaps(events) {
                continue;
            }
            let trigger = ObjectId(index);
            let key = (trigger, events);
            if fired.contains(&key) {
                continue;
            }
            fired.push(key);
            effects.read(trigger);
            for statement in body {
                self.reads(&relation_names(statement), effects);
                match row_write(statement) {
                    Some(RowWrite::Table(name, events)) => self.write(name, events, effects, fired),
                    Some(RowWrite::Untracked) => effects.persistent_write = true,
                    None => {}
                }
            }
        }
    }

    /// Drops `id` and every trigger and index attached to it.
    fn retire(&mut self, id: ObjectId, effects: &mut Effects) {
        if !self.objects[id.0].alive {
            return;
        }
        self.objects[id.0].alive = false;
        effects.touch(id);
        let attached: Vec<ObjectId> = self
            .objects
            .iter()
            .enumerate()
            .filter(|(_, object)| {
                object.alive
                    && match &object.shape {
                        Shape::Trigger { owner: Owner::Connection(owner), .. }
                        | Shape::Index { owner } => *owner == id,
                        _ => false,
                    }
            })
            .map(|(index, _)| ObjectId(index))
            .collect();
        for dependent in attached {
            self.triggers.retain(|_, value| *value != dependent);
            self.indexes.retain(|_, value| *value != dependent);
            self.retire(dependent, effects);
        }
    }
}

impl ObjectId {
    pub(crate) fn index(self) -> usize {
        self.0
    }
}

/// The rows a statement writes.
enum RowWrite<'a> {
    /// Rows of the named table or view, raising `Events`.
    Table(&'a ObjectName, Events),
    /// Rows of something this analysis cannot name, which is treated as
    /// file storage.
    Untracked,
}

/// What `statement` writes, or `None` when it writes no rows.
#[cfg_attr(feature = "std", recursive::recursive)]
fn row_write(statement: &Statement) -> Option<RowWrite<'_>> {
    match statement {
        Statement::Insert(insert) => {
            let events = match &insert.on {
                Some(OnInsert::OnConflict(conflict))
                    if matches!(conflict.action, OnConflictAction::DoUpdate(_)) =>
                {
                    Events::UPSERT
                }
                _ => Events::INSERT,
            };
            Some(match &insert.table {
                TableObject::TableName(name) => RowWrite::Table(name, events),
                TableObject::TableFunction(_) | TableObject::TableQuery(_) => RowWrite::Untracked,
            })
        }
        Statement::Update(update) => {
            Some(match &update.table.relation {
                TableFactor::Table { name, .. } => RowWrite::Table(name, Events::UPDATE),
                _ => RowWrite::Untracked,
            })
        }
        Statement::Delete(delete) => {
            let (FromTable::WithFromKeyword(tables) | FromTable::WithoutKeyword(tables)) =
                &delete.from;
            Some(match tables.first().map(|table| &table.relation) {
                Some(TableFactor::Table { name, .. }) => RowWrite::Table(name, Events::DELETE),
                _ => RowWrite::Untracked,
            })
        }
        Statement::Merge(_) => Some(RowWrite::Untracked),
        Statement::Query(query) => {
            match query.body.as_ref() {
                SetExpr::Insert(statement)
                | SetExpr::Update(statement)
                | SetExpr::Delete(statement) => row_write(statement),
                SetExpr::Merge(_) => Some(RowWrite::Untracked),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Whether a created relation lives in the connection's `temp` schema.
fn is_temporary(flagged: bool, name: &ObjectName) -> bool {
    flagged
        || split(name)
            .and_then(|(schema, _)| schema)
            .is_some_and(|schema| schema.eq_ignore_ascii_case("temp"))
}

/// The schema qualifier and the SQLite-folded object name.
fn split(name: &ObjectName) -> Option<(Option<String>, String)> {
    let mut parts = name.0.iter().filter_map(ObjectNamePart::as_ident);
    let last = parts.next_back()?;
    let schema = parts.next_back().map(|schema| schema.value.clone());
    Some((schema, last.value.to_ascii_lowercase()))
}
