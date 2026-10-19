//! Ordered field identities retained across schema mutations.

use alloc::{
    collections::{BTreeMap, BTreeSet},
    string::String,
    vec::Vec,
};

use sqlparser::ast::Ident;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct ColumnId(usize);

#[derive(Debug, Clone, Default)]
pub(super) struct Columns {
    names: BTreeMap<String, ColumnId>,
    slots: Vec<Option<Ident>>,
    hidden: BTreeSet<ColumnId>,
}

impl Columns {
    pub(super) fn from_names(names: impl IntoIterator<Item = Ident>) -> Self {
        let mut columns = Self::default();
        for name in names {
            columns.add(name);
        }
        columns
    }

    pub(super) fn add(&mut self, name: Ident) {
        let id = ColumnId(self.slots.len());
        self.names.insert(name.value.to_ascii_lowercase(), id);
        self.slots.push(Some(name));
    }

    pub(super) fn add_hidden(&mut self, name: Ident) {
        self.add(name);
        self.hidden.insert(ColumnId(self.slots.len() - 1));
    }

    pub(super) fn rename_prefix<'a>(&mut self, names: impl IntoIterator<Item = &'a Ident>) {
        let slots = self.slots.iter_mut().enumerate().filter_map(|(index, slot)| {
            (!self.hidden.contains(&ColumnId(index))).then_some(slot).filter(|slot| slot.is_some())
        });
        for (slot, name) in slots.zip(names) {
            *slot = Some(name.clone());
        }
        self.rebuild_names();
    }

    pub(super) fn visible(&self) -> impl Iterator<Item = &Ident> {
        self.iter().filter_map(|(id, name)| (!self.hidden.contains(&id)).then_some(name))
    }

    fn rebuild_names(&mut self) {
        self.names.clear();
        for (index, slot) in self.slots.iter().enumerate() {
            if let Some(name) = slot {
                self.names.insert(name.value.to_ascii_lowercase(), ColumnId(index));
            }
        }
    }

    pub(super) fn rename(&mut self, from: &Ident, to: &Ident) {
        if let Some(id) = self.names.remove(&from.value.to_ascii_lowercase()) {
            self.names.insert(to.value.to_ascii_lowercase(), id);
            self.slots[id.0] = Some(to.clone());
        }
    }

    pub(super) fn remove(&mut self, name: &Ident) {
        if let Some(id) = self.names.remove(&name.value.to_ascii_lowercase()) {
            self.slots[id.0] = None;
        }
    }

    pub(super) fn lookup(&self, name: &str) -> Option<ColumnId> {
        self.names.get(name).copied()
    }

    pub(super) fn get(&self, id: ColumnId) -> Option<&Ident> {
        self.slots.get(id.0).and_then(Option::as_ref)
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = (ColumnId, &Ident)> {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(index, name)| name.as_ref().map(|name| (ColumnId(index), name)))
    }

    pub(super) fn replace_names(&mut self, names: impl IntoIterator<Item = Ident>) {
        let mut names = names.into_iter();
        for slot in &mut self.slots {
            if slot.is_some() {
                *slot = names.next();
            }
        }
        self.names.clear();
        for (index, slot) in self.slots.iter().enumerate() {
            if let Some(name) = slot {
                self.names.insert(name.value.to_ascii_lowercase(), ColumnId(index));
            }
        }
        for name in names {
            self.add(name);
        }
    }
}
