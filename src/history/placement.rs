//! One stage over the emitted units that decides each object's home file.
//!
//! A permanent view or trigger binds unqualified names to its own database and
//! may not name another, so whatever reads attached storage, directly or
//! through such a view, is emitted `TEMP`. Inner names stay unqualified, which
//! is sound because the collision check keeps names unique across all three.

#[cfg(feature = "std")]
use alloc::collections::{BTreeMap, BTreeSet};
#[cfg(not(feature = "std"))]
use alloc::{
    borrow::ToOwned,
    collections::{BTreeMap, BTreeSet},
    format,
    string::String,
    vec,
    vec::Vec,
};

use sql_traits::{
    structs::ParserDB,
    traits::{DatabaseLike, TableLike},
};
use sqlparser::ast::{
    AlterTableOperation, ColumnOption, CreateTable, FromTable, ObjectName, ObjectNamePart,
    ObjectType, RenameTableNameKind, SetExpr, Statement, TableConstraint, TableFactor, TableObject,
};

use crate::{
    errors::Error,
    impls::{
        object_name::{last_ident_value_or_display, quoted_ident},
        scoped_relations::relation_names,
        translator_impls::rls::resolve_trigger_table_name,
    },
    manifest::Placement,
    options::{AttachedTables, Pg2SqliteOptions},
};

/// The database files an initialization belongs to, as a set over the main,
/// the attached and the connection-local `temp` storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Files(u8);

impl Files {
    /// The main database file.
    pub(crate) const MAIN: Self = Self(1);
    /// The attached database file.
    pub(crate) const ATTACHED: Self = Self(2);
    /// The connection-local `temp` storage.
    pub(crate) const CONNECTION: Self = Self(4);

    /// Whether `other` is a subset of `self`.
    #[must_use]
    pub(crate) const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Whether the home of a `placement` is among the replayed files.
    #[must_use]
    pub(crate) fn contains_placement(self, placement: Placement) -> bool {
        match placement {
            Placement::Main => self.contains(Self::MAIN),
            Placement::Attached => self.contains(Self::ATTACHED),
            Placement::Temp => self.contains(Self::CONNECTION),
        }
    }
}

impl core::ops::BitOr for Files {
    type Output = Self;
    fn bitor(self, other: Self) -> Self::Output {
        Self(self.0 | other.0)
    }
}

/// Where each created table, view and index lives, keyed as SQLite folds
/// names.
pub(crate) type Homes = BTreeMap<String, Placement>;

/// Where the table, view or index named `name` lives, `Main` for an object
/// the batch does not create.
pub(crate) fn home(homes: &Homes, name: &str) -> Placement {
    homes.get(&key(name)).copied().unwrap_or(Placement::Main)
}

/// Where the table, view or index `name` lives, `Main` for an object the
/// batch does not create.
pub(crate) fn home_of(homes: &Homes, name: &ObjectName) -> Placement {
    homes.get(&key_of(name)).copied().unwrap_or(Placement::Main)
}

/// The placement of every object, with the emitted statements qualified and
/// marked to match.
pub(crate) struct Placed {
    /// The home of each created object, keyed by its SQLite-folded name.
    pub(crate) homes: Homes,
    /// Whether any attached tables are configured.
    pub(crate) attached_configured: bool,
    /// The files each source unit's objects live in.
    pub(crate) unit_files: Vec<Files>,
    /// The home of each source table's logical name, for a statement that
    /// writes a table the batch does not create.
    pub(crate) logical_storage: Homes,
    /// The home of each created trigger, keyed by its SQLite-folded name.
    pub(crate) trigger_homes: Homes,
    /// The name the attached database is attached under, empty when none.
    pub(crate) attached_schema: String,
}

impl Placed {
    /// Where the table, view or index named `name` lives.
    pub(crate) fn home(&self, name: &str) -> Placement {
        home(&self.homes, name)
    }

    /// Places a rebound statement, keeping the home of the object it creates.
    pub(crate) fn place_rebound(&mut self, statement: &mut Statement) -> Result<Placement, Error> {
        place_statement(
            statement,
            &self.attached_schema,
            &self.homes,
            &self.logical_storage,
            &mut self.trigger_homes,
        )
    }

    /// Whether the object a statement creates, changes or writes lives in the
    /// connection's `temp` storage.
    pub(crate) fn is_connection_local(&self, statement: &Statement) -> bool {
        is_connection_local(statement, &self.homes, &self.logical_storage, &self.trigger_homes)
    }
}

/// Places objects while retaining source execution order.
pub(crate) fn place(
    sources: &[Statement],
    emitted: &mut [Vec<Statement>],
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Placed, Error> {
    let attached = options.get_attached_tables();
    let storage = match attached {
        Some(attached) => attached_storage(sources, emitted, attached, schema, options)?,
        None => BTreeSet::new(),
    };
    let mut homes = homes(sources, emitted, schema, &storage);
    let mut logical_storage = Homes::new();
    for table in schema.tables() {
        let physical = resolve_trigger_table_name(table.table_name(), table, schema, options)?;
        let physical_key = key(&physical);
        // Nothing the batch emits relocates a table it does not create.
        let placement = homes.get(&physical_key).copied().unwrap_or_else(|| {
            if table.temporary {
                Placement::Temp
            } else if storage.contains(&physical_key) {
                Placement::Attached
            } else {
                Placement::Main
            }
        });
        homes.insert(physical_key, placement);
        logical_storage.insert(key(table.table_name()), placement);
    }
    let mut trigger_homes = Homes::new();
    let mut placed_unit_files = Vec::with_capacity(sources.len());
    let attached_schema =
        attached.map_or_else(String::new, |attached| attached.schema().to_owned());
    for (source, statements) in sources.iter().zip(emitted.iter_mut()) {
        let files = unit_files(
            source,
            statements,
            &attached_schema,
            &homes,
            &logical_storage,
            &mut trigger_homes,
        )?;
        placed_unit_files.push(files);
    }
    Ok(Placed {
        homes,
        attached_configured: attached.is_some(),
        unit_files: placed_unit_files,
        logical_storage,
        trigger_homes,
        attached_schema,
    })
}

/// The fresh-file selections that replay a source unit.
///
/// A fresh-file initialization replays a unit only when it creates or writes
/// no storage of the file that stays in place, so a definition or a seed
/// write aimed at the other file is not run a second time.
fn unit_files(
    source: &Statement,
    emitted: &mut [Statement],
    attached_schema: &str,
    homes: &Homes,
    logical_storage: &Homes,
    trigger_homes: &mut Homes,
) -> Result<Files, Error> {
    if is_transaction_control(source) {
        return Ok(Files::MAIN | Files::ATTACHED);
    }
    let mut touches_main = false;
    let mut touches_attached = false;
    for statement in emitted.iter_mut() {
        match place_statement(statement, attached_schema, homes, logical_storage, trigger_homes)? {
            Placement::Main => touches_main = true,
            Placement::Attached => touches_attached = true,
            Placement::Temp => {}
        }
    }
    let mut files = Files::default();
    if !touches_attached {
        files = files | Files::MAIN;
    }
    if !touches_main {
        files = files | Files::ATTACHED;
    }
    Ok(files)
}

/// Whether `source` is a transaction control statement.
fn is_transaction_control(statement: &Statement) -> bool {
    matches!(
        statement,
        Statement::StartTransaction { .. }
            | Statement::Commit { .. }
            | Statement::Rollback { .. }
            | Statement::Savepoint { .. }
            | Statement::ReleaseSavepoint { .. }
    )
}

/// The SQLite names of each listed table's storage and its generated virtual
/// tables.
fn attached_storage(
    inputs: &[Statement],
    translated: &[Vec<Statement>],
    attached: &AttachedTables,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<BTreeSet<String>, Error> {
    let name = attached.schema();
    if name.is_empty() || name.eq_ignore_ascii_case("main") || name.eq_ignore_ascii_case("temp") {
        return Err(unusable(format!(
            "`{name}` cannot name an attached database, since SQLite reserves `main` and `temp` \
             and an attached database needs a name"
        )));
    }

    let mut storage = BTreeSet::new();
    for listed in attached.tables() {
        let table = schema
            .tables()
            .find(|table| table.table_name().eq_ignore_ascii_case(listed))
            .ok_or_else(|| {
            unusable(format!(
                "`{listed}` is listed for the attached schema `{name}`, but the input \
                     declares no table by that name"
            ))
        })?;
        if table.temporary {
            return Err(unusable(format!(
                "`{listed}` is temporary and cannot store its rows in attached schema `{name}`"
            )));
        }
        storage.insert(key(&resolve_trigger_table_name(
            table.table_name(),
            table,
            schema,
            options,
        )?));
    }

    for (input, emitted) in inputs.iter().zip(translated) {
        reject_rename(input, attached)?;
        let Some(subject) = subject_table(input) else {
            continue;
        };
        if !attached.contains(&subject) {
            continue;
        }
        for statement in emitted {
            match statement {
                Statement::CreateTable(create) => {
                    storage.insert(key_of(&create.name));
                }
                Statement::CreateVirtualTable { name, .. } => {
                    storage.insert(key_of(name));
                }
                Statement::CreateIndex(_)
                | Statement::CreateView(_)
                | Statement::CreateTrigger(_)
                | Statement::AlterTable(_)
                | Statement::Drop { .. }
                | Statement::DropTrigger(_)
                | Statement::Insert(_)
                | Statement::Update(_)
                | Statement::Delete(_) => {}
                other => {
                    return Err(unusable(format!(
                        "`{input}` on the listed table `{subject}` translates to `{other}`, which \
                         names the table in a way the attached schema `{name}` cannot qualify"
                    )));
                }
            }
        }
    }
    Ok(storage)
}

/// The source table whose storage an input statement creates or extends.
fn subject_table(input: &Statement) -> Option<String> {
    match input {
        Statement::CreateTable(create) => Some(last_ident_value_or_display(&create.name)),
        Statement::CreateIndex(index) => Some(last_ident_value_or_display(&index.table_name)),
        Statement::AlterTable(alter) => Some(last_ident_value_or_display(&alter.name)),
        _ => None,
    }
}

/// Refuses renaming a table to or from a listed name.
fn reject_rename(input: &Statement, attached: &AttachedTables) -> Result<(), Error> {
    let renames: Vec<(&ObjectName, &ObjectName)> = match input {
        Statement::AlterTable(alter) => {
            alter
                .operations
                .iter()
                .filter_map(|operation| {
                    match operation {
                        AlterTableOperation::RenameTable {
                            table_name: RenameTableNameKind::As(to) | RenameTableNameKind::To(to),
                        } => Some((&alter.name, to)),
                        _ => None,
                    }
                })
                .collect()
        }
        Statement::RenameTable(renames) => {
            renames.iter().map(|rename| (&rename.old_name, &rename.new_name)).collect()
        }
        _ => return Ok(()),
    };
    for (from, to) in renames {
        let (from, to) = (last_ident_value_or_display(from), last_ident_value_or_display(to));
        if attached.contains(&from) || attached.contains(&to) {
            return Err(unusable(format!(
                "`{from}` is renamed to `{to}`, and the list for the attached schema `{}` names \
                 the table by one of the two. Create the table under its final name",
                attached.schema()
            )));
        }
    }
    Ok(())
}

/// Where every created table, view and index lives, with views closed over
/// `temp`.
fn homes(
    inputs: &[Statement],
    translated: &[Vec<Statement>],
    schema: &ParserDB,
    storage: &BTreeSet<String>,
) -> Homes {
    let mut homes = Homes::new();
    let mut views = Vec::new();
    for (input, emitted) in inputs.iter().zip(translated) {
        let temporary = subject_table(input).is_some_and(|subject| {
            schema
                .tables()
                .any(|table| table.table_name().eq_ignore_ascii_case(&subject) && table.temporary)
        });
        for statement in emitted {
            let table = match statement {
                Statement::CreateTable(create) => {
                    if create.temporary {
                        homes.insert(key_of(&create.name), Placement::Temp);
                        continue;
                    }
                    &create.name
                }
                Statement::CreateVirtualTable { name, .. } => name,
                Statement::CreateView(view) => {
                    let placement = if view.temporary { Placement::Temp } else { Placement::Main };
                    homes.insert(key_of(&view.name), placement);
                    views.push((key_of(&view.name), relation_names(&view.query)));
                    continue;
                }
                _ => continue,
            };
            let name = key_of(table);
            let placement = if temporary {
                Placement::Temp
            } else if storage.contains(&name) {
                Placement::Attached
            } else {
                Placement::Main
            };
            homes.insert(name, placement);
        }
    }
    for statement in translated.iter().flatten() {
        if let Statement::CreateIndex(index) = statement
            && let Some(name) = &index.name
        {
            let placement = homes.get(&key_of(&index.table_name)).copied();
            homes.insert(key_of(name), placement.unwrap_or(Placement::Main));
        }
    }

    loop {
        let mut grew = false;
        for (name, reads) in &views {
            if homes.get(name) != Some(&Placement::Temp) && reads_placed(reads, &homes) {
                homes.insert(name.clone(), Placement::Temp);
                grew = true;
            }
        }
        if !grew {
            return homes;
        }
    }
}

/// The home of the object a statement creates, changes or writes.
fn statement_placement(
    statement: &Statement,
    homes: &Homes,
    logical_storage: &Homes,
    trigger_homes: &Homes,
) -> Placement {
    let home = |name: &ObjectName| home_of(homes, name);
    match statement {
        Statement::CreateTrigger(trigger) => {
            let reads = relation_names(trigger);
            let table = home(&trigger.table_name);
            if table == Placement::Main && !trigger.temporary && !reads_placed(&reads, homes) {
                Placement::Main
            } else {
                Placement::Temp
            }
        }
        Statement::CreateTable(create) => home(&create.name),
        Statement::CreateVirtualTable { name, .. } => home(name),
        Statement::CreateIndex(index) => home(&index.table_name),
        Statement::AlterTable(alter) => home(&alter.name),
        Statement::CreateView(view) => home(&view.name),
        Statement::Drop {
            object_type: ObjectType::Table | ObjectType::View | ObjectType::Index,
            names,
            ..
        } => names.first().map_or(Placement::Main, home),
        Statement::DropTrigger(drop) => {
            trigger_homes.get(&key_of(&drop.trigger_name)).copied().unwrap_or(Placement::Main)
        }
        other => {
            primary_write_target(other).map_or(Placement::Main, |name| {
                logical_storage.get(&key_of(name)).copied().unwrap_or_else(|| home(name))
            })
        }
    }
}

/// Whether the object a statement creates, changes or writes lives in the
/// connection's `temp` storage, which is all a connection program replays.
pub(crate) fn is_connection_local(
    statement: &Statement,
    homes: &Homes,
    logical_storage: &Homes,
    trigger_homes: &Homes,
) -> bool {
    statement_placement(statement, homes, logical_storage, trigger_homes) == Placement::Temp
}

/// Places one statement, qualifying or marking `TEMP` the object it creates.
fn place_statement(
    statement: &mut Statement,
    schema: &str,
    homes: &Homes,
    logical_storage: &Homes,
    trigger_homes: &mut Homes,
) -> Result<Placement, Error> {
    let placement = statement_placement(statement, homes, logical_storage, trigger_homes);
    let home = |name: &ObjectName| home_of(homes, name);
    Ok(match statement {
        Statement::CreateTrigger(trigger) => {
            if placement == Placement::Temp {
                trigger.temporary = true;
                match home(&trigger.table_name) {
                    Placement::Main => trigger.table_name = qualified(&trigger.table_name, "main"),
                    Placement::Attached => {
                        trigger.table_name = qualified(&trigger.table_name, schema);
                    }
                    Placement::Temp => {}
                }
            }
            trigger_homes.entry(key_of(&trigger.name)).or_insert(placement);
            placement
        }
        Statement::CreateTable(create) => {
            reject_foreign_keys_across(create, schema, homes)?;
            qualify_if_attached(&mut create.name, placement, schema);
            create.temporary |= placement == Placement::Temp;
            placement
        }
        Statement::CreateVirtualTable { name, .. } => {
            match placement {
                Placement::Attached => *name = qualified(name, schema),
                Placement::Temp => *name = qualified(name, "temp"),
                Placement::Main => {}
            }
            placement
        }
        Statement::CreateIndex(index) => {
            if let Some(name) = &mut index.name {
                match placement {
                    Placement::Attached => *name = qualified(name, schema),
                    Placement::Temp => *name = qualified(name, "temp"),
                    Placement::Main => {}
                }
            }
            placement
        }
        Statement::AlterTable(alter) => {
            for operation in &alter.operations {
                if let AlterTableOperation::AddColumn { column_def, .. } = operation {
                    for option in &column_def.options {
                        if let ColumnOption::ForeignKey(foreign) = &option.option
                            && (home(&foreign.foreign_table) == Placement::Attached)
                                != (placement == Placement::Attached)
                        {
                            return Err(unusable(format!(
                                "the foreign key from `{}` to `{}` crosses database boundaries",
                                alter.name, foreign.foreign_table
                            )));
                        }
                    }
                }
            }
            qualify_if_attached(&mut alter.name, placement, schema);
            placement
        }
        Statement::CreateView(view) => {
            if placement == Placement::Temp {
                view.temporary = true;
            }
            placement
        }
        Statement::Drop {
            object_type: ObjectType::Table | ObjectType::View | ObjectType::Index,
            names,
            ..
        } => {
            for name in names.iter_mut() {
                qualify_if_attached(name, placement, schema);
            }
            placement
        }
        _ => placement,
    })
}

#[cfg_attr(feature = "std", recursive::recursive)]
pub(super) fn primary_write_target(statement: &Statement) -> Option<&ObjectName> {
    fn factor(factor: &TableFactor) -> Option<&ObjectName> {
        match factor {
            TableFactor::Table { name, .. } => Some(name),
            _ => None,
        }
    }
    match statement {
        Statement::Insert(insert) => {
            match &insert.table {
                TableObject::TableName(name) => Some(name),
                _ => None,
            }
        }
        Statement::Update(update) => factor(&update.table.relation),
        Statement::Delete(delete) => {
            let tables = match &delete.from {
                FromTable::WithFromKeyword(tables) | FromTable::WithoutKeyword(tables) => tables,
            };
            tables.first().and_then(|table| factor(&table.relation))
        }
        Statement::Query(query) => {
            match query.body.as_ref() {
                SetExpr::Insert(statement)
                | SetExpr::Update(statement)
                | SetExpr::Delete(statement)
                | SetExpr::Merge(statement) => primary_write_target(statement),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Refuses a foreign key across databases, which SQLite would leave unenforced.
fn reject_foreign_keys_across(
    create: &CreateTable,
    schema: &str,
    homes: &Homes,
) -> Result<(), Error> {
    let attached = |name: &ObjectName| homes.get(&key_of(name)) == Some(&Placement::Attached);
    let child_attached = attached(&create.name);
    let parents = create
        .columns
        .iter()
        .flat_map(|column| &column.options)
        .filter_map(|option| {
            match &option.option {
                ColumnOption::ForeignKey(key) => Some(&key.foreign_table),
                _ => None,
            }
        })
        .chain(create.constraints.iter().filter_map(|constraint| {
            match constraint {
                TableConstraint::ForeignKey(key) => Some(&key.foreign_table),
                _ => None,
            }
        }));
    for parent in parents {
        if attached(parent) != child_attached {
            let (inside, outside) =
                if child_attached { (&create.name, parent) } else { (parent, &create.name) };
            return Err(unusable(format!(
                "`{}` references `{parent}` with a foreign key, and `{inside}` lives in the \
                 attached schema `{schema}` while `{outside}` lives in `main`. SQLite looks a \
                 parent up in the child's own database, so the key would go unenforced. List \
                 both tables or neither",
                create.name
            )));
        }
    }
    Ok(())
}

fn qualify_if_attached(name: &mut ObjectName, placement: Placement, schema: &str) {
    if placement == Placement::Attached {
        *name = qualified(name, schema);
    }
}

/// `name` written in `schema`.
fn qualified(name: &ObjectName, schema: &str) -> ObjectName {
    let mut parts = vec![ObjectNamePart::Identifier(quoted_ident(schema))];
    parts.extend(name.0.last().cloned());
    ObjectName(parts)
}

/// Whether any of `reads` lives in the attached database or in `temp`.
fn reads_placed(reads: &[String], homes: &Homes) -> bool {
    reads.iter().any(|name| homes.get(name).is_some_and(|placement| *placement != Placement::Main))
}

fn key(name: &str) -> String {
    name.to_ascii_lowercase()
}

fn key_of(name: &ObjectName) -> String {
    key(&last_ident_value_or_display(name))
}

fn unusable(reason: String) -> Error {
    Error::UnusableAttachedTables { reason }
}
