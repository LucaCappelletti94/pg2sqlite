//! Splits the emitted statements between `main`, an attached database and
//! `temp`.
//!
//! A permanent view or trigger binds unqualified names to its own database and
//! may not name another, so whatever reads attached storage, directly or
//! through such a view, is emitted `TEMP`. Inner names stay unqualified, which
//! is sound because the collision check keeps names unique across all three.

use alloc::{
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

/// Object placement without changing bootstrap statement order.
pub(crate) struct Placed {
    pub(crate) bootstrap: Vec<Statement>,
    pub(crate) scopes: Vec<Placement>,
    /// Final physical tables stored in `main`.
    pub(crate) main_tables: BTreeSet<String>,
    /// Final physical tables stored in the attached database.
    pub(crate) attached_tables: BTreeSet<String>,
    homes: Homes,
}

impl Placed {
    /// Where the table, view or index named `name` lives.
    pub(crate) fn home(&self, name: &str) -> Placement {
        self.homes.get(&key(name)).copied().unwrap_or(Placement::Main)
    }
}

/// Where each created table, view and index lives, keyed as SQLite folds names.
type Homes = BTreeMap<String, Placement>;

/// Places objects while retaining source execution order.
pub(crate) fn place(
    inputs: &[Statement],
    translated: Vec<Vec<Statement>>,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Placed, Error> {
    let attached = options.get_attached_tables();
    let storage = match attached {
        Some(attached) => attached_storage(inputs, &translated, attached, schema, options)?,
        None => BTreeSet::new(),
    };
    let mut homes = homes(inputs, &translated, schema, &storage);
    let mut logical_storage = Homes::new();
    let mut main_tables = BTreeSet::new();
    let mut attached_tables = BTreeSet::new();
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
        if !table.temporary {
            match placement {
                Placement::Main => {
                    main_tables.insert(physical_key.clone());
                }
                Placement::Attached => {
                    attached_tables.insert(physical_key.clone());
                }
                Placement::Temp => {}
            }
        }
        homes.insert(physical_key, placement);
        logical_storage.insert(key(table.table_name()), placement);
    }
    for name in surviving_virtual_tables(translated.iter().flatten()) {
        match homes.get(&name).copied().unwrap_or(Placement::Main) {
            Placement::Main => {
                main_tables.insert(name);
            }
            Placement::Attached => {
                attached_tables.insert(name);
            }
            Placement::Temp => {}
        }
    }
    let capacity = translated.iter().map(Vec::len).sum();
    let mut bootstrap = Vec::with_capacity(capacity);
    let mut scopes = Vec::with_capacity(capacity);
    let mut trigger_homes = Homes::new();
    let attached_schema = attached.map_or("", AttachedTables::schema);
    for mut statement in translated.into_iter().flatten() {
        let scope = place_statement(
            &mut statement,
            attached_schema,
            &homes,
            &logical_storage,
            &mut trigger_homes,
        )?;
        scopes.push(scope);
        bootstrap.push(statement);
    }
    Ok(Placed { bootstrap, scopes, main_tables, attached_tables, homes })
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
        let Some(subject) = subject_table(input) else { continue };
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

/// The virtual tables the batch creates and does not drop again.
fn surviving_virtual_tables<'a>(
    inputs: impl IntoIterator<Item = &'a Statement>,
) -> BTreeSet<String> {
    let mut alive = BTreeSet::new();
    for input in inputs {
        match input {
            Statement::CreateVirtualTable { name, .. } => {
                alive.insert(key_of(name));
            }
            Statement::Drop { object_type: ObjectType::Table, temporary: false, names, .. } => {
                for name in names {
                    alive.remove(&key_of(name));
                }
            }
            _ => {}
        }
    }
    alive
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

/// Places one statement, qualifying or marking `TEMP` the object it creates.
fn place_statement(
    statement: &mut Statement,
    schema: &str,
    homes: &Homes,
    logical_storage: &Homes,
    trigger_homes: &mut Homes,
) -> Result<Placement, Error> {
    let home = |name: &ObjectName| homes.get(&key_of(name)).copied().unwrap_or(Placement::Main);
    Ok(match statement {
        Statement::CreateTrigger(trigger) => {
            let reads = relation_names(&*trigger);
            let table = home(&trigger.table_name);
            let placement =
                if table == Placement::Main && !trigger.temporary && !reads_placed(&reads, homes) {
                    Placement::Main
                } else {
                    trigger.temporary = true;
                    match table {
                        Placement::Main => {
                            trigger.table_name = qualified(&trigger.table_name, "main");
                        }
                        Placement::Attached => {
                            trigger.table_name = qualified(&trigger.table_name, schema);
                        }
                        Placement::Temp => {}
                    }
                    Placement::Temp
                };
            trigger_homes.insert(key_of(&trigger.name), placement);
            placement
        }
        Statement::CreateTable(create) => {
            reject_foreign_keys_across(create, schema, homes)?;
            let placement = home(&create.name);
            qualify_if_attached(&mut create.name, placement, schema);
            create.temporary |= placement == Placement::Temp;
            placement
        }
        Statement::CreateVirtualTable { name, .. } => {
            let placement = home(name);
            match placement {
                Placement::Attached => *name = qualified(name, schema),
                Placement::Temp => *name = qualified(name, "temp"),
                Placement::Main => {}
            }
            placement
        }
        Statement::CreateIndex(index) => {
            let placement = home(&index.table_name);
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
            let placement = home(&alter.name);
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
            let placement = home(&view.name);
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
            let placement = names.first().map_or(Placement::Main, home);
            for name in names.iter_mut() {
                qualify_if_attached(name, placement, schema);
            }
            placement
        }
        Statement::DropTrigger(drop) => {
            trigger_homes.get(&key_of(&drop.trigger_name)).copied().unwrap_or(Placement::Main)
        }
        other => {
            primary_write_target(other).map_or(Placement::Main, |name| {
                logical_storage.get(&key_of(name)).copied().unwrap_or_else(|| home(name))
            })
        }
    })
}

fn primary_write_target(statement: &Statement) -> Option<&ObjectName> {
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
