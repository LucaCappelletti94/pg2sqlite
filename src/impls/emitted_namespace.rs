//! The names the emitted script defines, and the ones that would collide.
//!
//! PostgreSQL gives every schema its own namespace and scopes a trigger name to
//! its table. SQLite has one namespace holding tables, views and indexes
//! together, and one holding every trigger in the database. So objects that
//! were distinct on the way in can arrive at the same name, and the script then
//! fails at apply, or silently keeps only the first when the create carries
//! `IF NOT EXISTS`.
//!
//! This walks the emitted statements keeping the namespaces SQLite itself would
//! keep, rather than comparing the input's qualified names, because two of the
//! collisions have nothing to do with schemas: a trigger name repeated on a
//! second table, and a generated row-security backing table landing on a
//! declared one.
//!
//! A `DROP` frees a name, and dropping a table or a view frees every index and
//! trigger attached to it, so each of those records what it hangs off. Without
//! that the walk would refuse input that translates and runs today.

use alloc::{collections::BTreeMap, vec::Vec};
#[cfg(not(feature = "std"))]
#[allow(unused_imports)]
use alloc::{
    format,
    string::{String, ToString},
};

use sqlparser::ast::{AlterTableOperation, ObjectName, ObjectType, RenameTableNameKind, Statement};

use super::object_name::last_ident_value_or_display;
use crate::errors::Error;

/// Where an emitted statement came from, for the message a collision raises.
#[derive(Clone, Copy)]
pub(crate) enum Source<'a> {
    /// The input statement it was translated from.
    Input(&'a Statement),
    /// Something the translation adds on its own.
    Generated(&'a str),
}

impl Source<'_> {
    /// Names the source the way the author would recognise it, which is the
    /// object it declared rather than the statement's whole text.
    fn describe(self) -> String {
        let statement = match self {
            Self::Generated(label) => return label.to_string(),
            Self::Input(statement) => statement,
        };
        match statement {
            Statement::CreateTable(create) => create.name.to_string(),
            Statement::CreateView(view) => view.name.to_string(),
            Statement::CreateVirtualTable { name, .. } => name.to_string(),
            Statement::CreateIndex(index) => {
                index.name.as_ref().map_or_else(
                    || format!("an index on {}", index.table_name),
                    ObjectName::to_string,
                )
            }
            Statement::CreateTrigger(trigger) => {
                format!("{} on {}", trigger.name, trigger.table_name)
            }
            Statement::AlterTable(alter) => alter.name.to_string(),
            other => other.to_string(),
        }
    }
}

/// Refuses the first emitted definition that lands on a name the script already
/// holds.
pub(crate) fn reject_name_collisions<'a>(
    emitted: impl IntoIterator<Item = (Source<'a>, &'a Statement)>,
) -> Result<(), Error> {
    let mut namespaces = Namespaces::default();
    for (source, statement) in emitted {
        namespaces.apply(statement, source)?;
    }
    Ok(())
}

/// One live name, and what would take it away.
struct Entry<'a> {
    source: Source<'a>,
    /// The table or view this hangs off, when dropping that drops this too.
    owner: Option<String>,
}

#[derive(Default)]
struct Namespaces<'a> {
    /// Tables, views and indexes, which SQLite keeps together.
    objects: BTreeMap<String, Entry<'a>>,
    /// Triggers, which SQLite keeps apart but names database-wide.
    triggers: BTreeMap<String, Entry<'a>>,
}

impl<'a> Namespaces<'a> {
    fn apply(&mut self, statement: &'a Statement, source: Source<'a>) -> Result<(), Error> {
        match statement {
            Statement::CreateTable(create) => self.define_object(&create.name, None, source),
            Statement::CreateView(view) => self.define_object(&view.name, None, source),
            Statement::CreateVirtualTable { name, .. } => self.define_object(name, None, source),
            Statement::CreateIndex(index) => {
                match &index.name {
                    Some(name) => self.define_object(name, Some(&index.table_name), source),
                    None => Ok(()),
                }
            }
            Statement::CreateTrigger(trigger) => {
                self.define_trigger(&trigger.name, &trigger.table_name, source)
            }
            Statement::Drop { object_type, names, .. } => {
                if matches!(object_type, ObjectType::Table | ObjectType::View | ObjectType::Index) {
                    for name in names {
                        self.drop_object(name);
                    }
                }
                Ok(())
            }
            Statement::DropTrigger(drop) => {
                self.triggers.remove(&key(&drop.trigger_name));
                Ok(())
            }
            Statement::AlterTable(alter) => {
                for operation in &alter.operations {
                    if let AlterTableOperation::RenameTable { table_name } = operation {
                        let (RenameTableNameKind::As(to) | RenameTableNameKind::To(to)) =
                            table_name;
                        self.rename_object(&alter.name, to, source)?;
                    }
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn define_object(
        &mut self,
        name: &ObjectName,
        owner: Option<&ObjectName>,
        source: Source<'a>,
    ) -> Result<(), Error> {
        let key = key(name);
        if let Some(held) = self.objects.get(&key) {
            return Err(object_collision(&key, held.source, source));
        }
        self.objects.insert(key, Entry { source, owner: owner.map(self::key) });
        Ok(())
    }

    fn define_trigger(
        &mut self,
        name: &ObjectName,
        owner: &ObjectName,
        source: Source<'a>,
    ) -> Result<(), Error> {
        let key = key(name);
        if let Some(held) = self.triggers.get(&key) {
            return Err(trigger_collision(&key, held.source, source));
        }
        self.triggers.insert(key, Entry { source, owner: Some(self::key(owner)) });
        Ok(())
    }

    /// Drops `name` and everything SQLite would drop with it.
    fn drop_object(&mut self, name: &ObjectName) {
        let key = key(name);
        self.objects.remove(&key);
        self.objects.retain(|_, entry| entry.owner.as_ref() != Some(&key));
        self.triggers.retain(|_, entry| entry.owner.as_ref() != Some(&key));
    }

    fn rename_object(
        &mut self,
        from: &ObjectName,
        to: &ObjectName,
        source: Source<'a>,
    ) -> Result<(), Error> {
        let (from, to) = (key(from), key(to));
        if let Some(held) = self.objects.get(&to) {
            return Err(object_collision(&to, held.source, source));
        }
        if let Some(entry) = self.objects.remove(&from) {
            self.objects.insert(to.clone(), entry);
        }
        for entry in self.objects.values_mut().chain(self.triggers.values_mut()) {
            if entry.owner.as_ref() == Some(&from) {
                entry.owner = Some(to.clone());
            }
        }
        Ok(())
    }
}

/// The name SQLite would file an object under, which folds ASCII case.
fn key(name: &ObjectName) -> String {
    last_ident_value_or_display(name).to_ascii_lowercase()
}

fn object_collision(name: &str, first: Source<'_>, second: Source<'_>) -> Error {
    Error::EmittedNameCollision {
        kind: "objects".to_string(),
        name: name.to_string(),
        first: first.describe(),
        second: second.describe(),
        reason: "SQLite holds tables, views and indexes in one namespace where PostgreSQL holds \
                 one per schema"
            .to_string(),
    }
}

fn trigger_collision(name: &str, first: Source<'_>, second: Source<'_>) -> Error {
    Error::EmittedNameCollision {
        kind: "triggers".to_string(),
        name: name.to_string(),
        first: first.describe(),
        second: second.describe(),
        reason: "SQLite names a trigger across the whole database where PostgreSQL names it \
                 within its table"
            .to_string(),
    }
}

/// Pairs each emitted statement with the input statement it came from.
///
/// A `CREATE TABLE` input can produce extra statements beyond the translated
/// table itself when RLS or a virtual extension is active. These extra
/// statements (views, triggers, virtual tables) are generated by the translator
/// and carry no direct author: they get `Source::Generated` with a label that
/// names the artifact kind rather than a fixed string.
pub(crate) fn sourced<'a>(
    inputs: &'a [Statement],
    translated: &'a [Vec<Statement>],
) -> impl Iterator<Item = (Source<'a>, &'a Statement)> {
    inputs.iter().zip(translated).flat_map(|(input, emitted)| {
        let is_create_table = matches!(input, Statement::CreateTable(_));
        emitted.iter().enumerate().map(move |(i, s)| {
            // The first statement is always the direct translation of the
            // input. Subsequent statements from a CREATE TABLE are
            // generated by the translator; label them by their
            // actual statement kind.
            let source = if is_create_table && i > 0 {
                Source::Generated(generated_artifact_label(s))
            } else {
                Source::Input(input)
            };
            (source, s)
        })
    })
}

/// A static label describing the kind of generated artifact, used in collision
/// error messages so the caller does not hunt for a statement they never wrote.
fn generated_artifact_label(statement: &Statement) -> &'static str {
    match statement {
        Statement::CreateVirtualTable { module_name, .. }
            if module_name.value.eq_ignore_ascii_case("vec0") =>
        {
            "a generated vec0 virtual table"
        }
        Statement::CreateVirtualTable { .. } => "a generated virtual table",
        Statement::CreateView(_) => "a generated RLS view",
        Statement::CreateTrigger(_) => "a generated trigger",
        Statement::CreateTable(_) => "a generated backing table",
        _ => "a generated statement",
    }
}
