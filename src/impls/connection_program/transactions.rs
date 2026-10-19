#[cfg(not(feature = "std"))]
use alloc::{
    format,
    string::{String, ToString},
};

use super::{Columns, Error, Ident, Rc, Storage, Walker, fold};

pub(super) struct Frame {
    name: Option<String>,
    step: usize,
    journal: usize,
}

pub(super) enum Undo {
    Name { storage: Storage, name: String, previous: Option<usize> },
    Created(usize),
    Retired(usize),
    Renamed { id: usize, display: String, folded: String },
    Columns { id: usize, columns: Rc<Columns> },
}

impl Walker {
    pub(super) fn open_transaction(&mut self) {
        self.next_tx += 1;
        self.tx = Some(self.next_tx);
        self.frames.push(Frame { name: None, step: self.steps.len(), journal: self.journal.len() });
    }

    pub(super) fn finish_transaction(&mut self, committed: bool) {
        if let Some(tx) = self.tx.take() {
            self.tx_committed.insert(tx, committed);
            if !committed {
                self.rollback_frame(0);
            }
            self.frames.clear();
            self.journal.clear();
        }
    }

    pub(super) fn savepoint(&mut self, name: &Ident) {
        if self.tx.is_some() {
            self.frames.push(Frame {
                name: Some(fold(name)),
                step: self.steps.len() + 1,
                journal: self.journal.len(),
            });
        }
    }

    fn named_frame(&self, name: &Ident, index: usize) -> Result<usize, Error> {
        let name = fold(name);
        self.frames.iter().rposition(|frame| frame.name.as_ref() == Some(&name)).ok_or_else(|| {
            Error::forward_refusal(format!("statement {index} names unavailable savepoint {name}"))
        })
    }

    pub(super) fn rollback_to(&mut self, name: &Ident, index: usize) -> Result<(), Error> {
        let position = self.named_frame(name, index)?;
        self.rollback_frame(position);
        self.frames.truncate(position + 1);
        Ok(())
    }

    pub(super) fn release(&mut self, name: &Ident, index: usize) -> Result<(), Error> {
        if self.tx.is_some() {
            let position = self.named_frame(name, index)?;
            self.frames.truncate(position);
        }
        Ok(())
    }

    fn rollback_frame(&mut self, position: usize) {
        let frame = &self.frames[position];
        for step in &mut self.steps[frame.step..] {
            step.undone = true;
        }
        let checkpoint = frame.journal;
        while self.journal.len() > checkpoint {
            match self.journal.pop().expect("the journal extends beyond its checkpoint") {
                Undo::Name { storage, name, previous } => {
                    let names = match storage {
                        Storage::Main => &mut self.main,
                        Storage::Temp => &mut self.temp,
                    };
                    if let Some(id) = previous {
                        names.insert(name, id);
                    } else {
                        names.remove(&name);
                    }
                }
                Undo::Created(id) => self.identities[id].dropped = true,
                Undo::Retired(id) => self.identities[id].dropped = false,
                Undo::Renamed { id, display, folded } => {
                    self.identities[id].final_name = display;
                    self.identities[id].final_name_folded = folded;
                }
                Undo::Columns { id, columns } => self.identities[id].columns = columns,
            }
        }
    }

    pub(super) fn insert_name(&mut self, storage: Storage, name: String, id: usize) {
        let key = self.tx.map(|_| name.clone());
        let names = match storage {
            Storage::Main => &mut self.main,
            Storage::Temp => &mut self.temp,
        };
        let previous = names.insert(name, id);
        if let Some(name) = key {
            self.journal.push(Undo::Name { storage, name, previous });
        }
    }

    pub(super) fn remove_name(&mut self, storage: Storage, name: &str) {
        let names = match storage {
            Storage::Main => &mut self.main,
            Storage::Temp => &mut self.temp,
        };
        if let Some(previous) = names.remove(name)
            && self.tx.is_some()
        {
            self.journal.push(Undo::Name {
                storage,
                name: name.to_string(),
                previous: Some(previous),
            });
        }
    }

    pub(super) fn record_creation(&mut self, id: usize) {
        if self.tx.is_some() {
            self.journal.push(Undo::Created(id));
        }
    }

    pub(super) fn retire_identity(&mut self, id: usize) {
        if self.tx.is_some() {
            self.journal.push(Undo::Retired(id));
        }
        self.identities[id].dropped = true;
    }

    pub(super) fn rename_identity(&mut self, id: usize, name: &str, folded: String) {
        let identity = &mut self.identities[id];
        if self.tx.is_some() {
            self.journal.push(Undo::Renamed {
                id,
                display: core::mem::replace(&mut identity.final_name, name.to_string()),
                folded: core::mem::replace(&mut identity.final_name_folded, folded),
            });
        } else {
            identity.final_name.clear();
            identity.final_name.push_str(name);
            identity.final_name_folded = folded;
        }
    }

    pub(super) fn checkpoint_columns(&mut self, id: usize) {
        if self.tx.is_some() {
            self.journal
                .push(Undo::Columns { id, columns: Rc::clone(&self.identities[id].columns) });
        }
    }
}
