//! Test for maintenance trigger translation.

use diesel::{Connection, QueryableByName, RunQueryDsl, SqliteConnection, prelude::*};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, Translator};
use sql_traits::structs::ParserDB;
use sqlparser::{ast::Statement, dialect::PostgreSqlDialect, parser::Parser};

// Schema definitions for test tables
diesel::table! {
    /// Brands table for testing maintenance triggers.
    brands (id) {
        /// Brand ID.
        id -> Integer,
        /// Brand name.
        name -> Text,
        /// Timestamp of last edit.
        edited_at -> Nullable<Text>,
    }
}

diesel::table! {
    /// Two-column table used in multi-assignment maintenance trigger tests.
    widgets (id) {
        /// Widget ID.
        id -> Integer,
        /// Maintenance-target text field.
        tag -> Nullable<Text>,
        /// Second maintenance-target field.
        slug -> Nullable<Text>,
    }
}

#[derive(Queryable, Selectable, Debug)]
#[diesel(table_name = widgets)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct Widget {
    tag: Option<String>,
    slug: Option<String>,
}

#[derive(Insertable)]
#[diesel(table_name = widgets)]
struct NewWidget {
    id: i32,
    tag: Option<String>,
    slug: Option<String>,
}

diesel::table! {
    /// Single-assignment table for the WHEN-clause merge test.
    w2 (id) {
        /// Row ID.
        id -> Integer,
        /// Tag field maintained by the trigger under test.
        tag -> Nullable<Text>,
    }
}

#[derive(Queryable, Selectable, Debug)]
#[diesel(table_name = w2)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct W2Row {
    tag: Option<String>,
}

#[derive(Insertable)]
#[diesel(table_name = w2)]
struct NewW2 {
    id: i32,
    tag: Option<String>,
}

/// A brand record with auto-updated edit timestamp.
#[derive(Queryable, Selectable)]
#[diesel(table_name = brands)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
#[allow(dead_code)]
struct Brand {
    /// Brand ID.
    id: i32,
    /// Brand name.
    name: String,
    /// Timestamp of last edit.
    edited_at: Option<String>,
}

/// Insertable brand record.
#[derive(Insertable)]
#[diesel(table_name = brands)]
struct NewBrand {
    /// Brand ID.
    id: i32,
    /// Brand name.
    name: String,
    /// Timestamp of last edit.
    edited_at: Option<String>,
}

#[derive(QueryableByName)]
struct TriggerDefinition {
    #[diesel(sql_type = diesel::sql_types::Text)]
    name: String,
    #[diesel(sql_type = diesel::sql_types::Text)]
    sql: String,
}

fn translate_with_direct_create_trigger_path(
    sql: &str,
    options: &Pg2SqliteOptions,
) -> Result<Vec<Statement>, Box<dyn std::error::Error>> {
    let pg_statements = Parser::parse_sql(&PostgreSqlDialect {}, sql)?;
    let schema = ParserDB::from_statements(pg_statements.clone(), "test".to_string())?;
    let mut translated = Vec::new();

    for statement in pg_statements {
        match statement {
            Statement::CreateTrigger(create_trigger) => {
                for (maybe_drop, translated_trigger) in
                    create_trigger.translate(&schema, options)?
                {
                    if let Some(drop_trigger) = maybe_drop {
                        translated.push(Statement::DropTrigger(drop_trigger));
                    }
                    translated.push(Statement::CreateTrigger(translated_trigger));
                }
            }
            other => translated.extend(other.translate(&schema, options)?),
        }
    }

    Ok(translated)
}

#[test]
fn test_maintenance_trigger() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
CREATE TABLE brands (
    id SERIAL PRIMARY KEY,
    name VARCHAR(255) NOT NULL,
    edited_at TEXT
);

CREATE OR REPLACE FUNCTION update_brands_edited_at() RETURNS TRIGGER AS $$
BEGIN
    NEW.edited_at = CURRENT_TIMESTAMP;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER trigger_update_brands_edited_at
BEFORE UPDATE ON brands
FOR EACH ROW EXECUTE FUNCTION update_brands_edited_at();
";

    let translator = Pg2Sqlite::default().sql(sql)?;
    let translated = translator.translate(&Pg2SqliteOptions::default())?;

    for stmt in &translated {
        println!("{stmt}");
    }

    let mut connection = SqliteConnection::establish(":memory:")?;

    // Setup SQLite environment
    diesel::sql_query("PRAGMA foreign_keys = ON").execute(&mut connection)?;

    // Run translations
    for stmt in translated {
        diesel::sql_query(stmt.to_string()).execute(&mut connection)?;
    }

    // Insert data
    diesel::insert_into(brands::table)
        .values(&NewBrand {
            id: 1,
            name: "Adidas".to_string(),
            edited_at: Some("2020-01-01".to_string()),
        })
        .execute(&mut connection)?;

    // Update data (trigger should fire)
    diesel::update(brands::table.filter(brands::id.eq(1)))
        .set(brands::name.eq("Nike"))
        .execute(&mut connection)?;

    // Verify by updating if condition is met
    let count = diesel::update(
        brands::table.filter(brands::id.eq(1).and(brands::edited_at.ne("2020-01-01"))),
    )
    .set(brands::name.eq("Verified"))
    .execute(&mut connection)?;

    assert_eq!(count, 1, "Expected 1 row updated, meaning edited_at changed from initial value");

    Ok(())
}

#[test]
fn test_maintenance_trigger_with_recursive_triggers_enabled()
-> Result<(), Box<dyn std::error::Error>> {
    let sql = "
CREATE TABLE brands (
    id SERIAL PRIMARY KEY,
    name VARCHAR(255) NOT NULL,
    edited_at TEXT
);

CREATE OR REPLACE FUNCTION update_brands_edited_at() RETURNS TRIGGER AS $$
BEGIN
    NEW.edited_at = CURRENT_TIMESTAMP;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER trigger_update_brands_edited_at
BEFORE UPDATE ON brands
FOR EACH ROW EXECUTE FUNCTION update_brands_edited_at();
";

    let translator = Pg2Sqlite::default().sql(sql)?;
    let translated = translator.translate(&Pg2SqliteOptions::default())?;

    let trigger_sql = translated
        .iter()
        .map(ToString::to_string)
        .find(|sql| sql.contains("CREATE TRIGGER trigger_update_brands_edited_at"))
        .expect("translated trigger statement should exist");
    assert!(
        trigger_sql.contains("AFTER UPDATE ON brands"),
        "maintenance trigger must be AFTER UPDATE: {trigger_sql}"
    );
    assert!(
        trigger_sql.contains("WHEN"),
        "maintenance trigger must carry a recursion-guard WHEN clause: {trigger_sql}"
    );

    let mut connection = SqliteConnection::establish(":memory:")?;
    diesel::sql_query("PRAGMA foreign_keys = ON").execute(&mut connection)?;
    diesel::sql_query("PRAGMA recursive_triggers = ON").execute(&mut connection)?;

    for stmt in translated {
        diesel::sql_query(stmt.to_string()).execute(&mut connection)?;
    }

    diesel::insert_into(brands::table)
        .values(&NewBrand {
            id: 1,
            name: "Adidas".to_string(),
            edited_at: Some("2020-01-01".to_string()),
        })
        .execute(&mut connection)?;

    diesel::update(brands::table.filter(brands::id.eq(1)))
        .set(brands::name.eq("Nike"))
        .execute(&mut connection)?;

    let updated =
        brands::table.filter(brands::id.eq(1)).select(Brand::as_select()).first(&mut connection)?;
    assert_eq!(updated.name, "Nike");

    Ok(())
}

#[test]
fn test_maintenance_trigger_before_insert() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
CREATE TABLE brands (
    id SERIAL PRIMARY KEY,
    name VARCHAR(255) NOT NULL,
    edited_at TEXT
);

CREATE OR REPLACE FUNCTION set_brands_edited_at() RETURNS TRIGGER AS $$
BEGIN
    NEW.edited_at = CURRENT_TIMESTAMP;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER trigger_insert_brands_edited_at
BEFORE INSERT ON brands
FOR EACH ROW EXECUTE FUNCTION set_brands_edited_at();
";

    let translator = Pg2Sqlite::default().sql(sql)?;
    let translated = translator.translate(&Pg2SqliteOptions::default())?;

    let trigger_sql = translated
        .iter()
        .map(ToString::to_string)
        .find(|sql| sql.contains("CREATE TRIGGER trigger_insert_brands_edited_at"))
        .expect("translated trigger statement should exist");
    assert!(
        trigger_sql.contains("AFTER INSERT ON brands"),
        "maintenance insert trigger should be translated to AFTER INSERT: {trigger_sql}"
    );

    let mut connection = SqliteConnection::establish(":memory:")?;
    diesel::sql_query("PRAGMA foreign_keys = ON").execute(&mut connection)?;
    diesel::sql_query("PRAGMA recursive_triggers = ON").execute(&mut connection)?;

    for stmt in translated {
        diesel::sql_query(stmt.to_string()).execute(&mut connection)?;
    }

    diesel::insert_into(brands::table)
        .values(&NewBrand { id: 1, name: "Adidas".to_string(), edited_at: None })
        .execute(&mut connection)?;

    let inserted =
        brands::table.filter(brands::id.eq(1)).select(Brand::as_select()).first(&mut connection)?;
    assert!(
        inserted.edited_at.is_some(),
        "edited_at should be populated by translated maintenance insert trigger"
    );

    Ok(())
}

#[test]
fn test_maintenance_trigger_before_insert_or_update_splits_trigger()
-> Result<(), Box<dyn std::error::Error>> {
    let sql = "
CREATE TABLE brands (
    id SERIAL PRIMARY KEY,
    name VARCHAR(255) NOT NULL,
    edited_at TEXT
);

CREATE OR REPLACE FUNCTION set_brands_edited_at() RETURNS TRIGGER AS $$
BEGIN
    NEW.edited_at = CURRENT_TIMESTAMP;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER trigger_upsert_brands_edited_at
BEFORE INSERT OR UPDATE ON brands
FOR EACH ROW EXECUTE FUNCTION set_brands_edited_at();
";

    let translator = Pg2Sqlite::default().sql(sql)?;
    let translated = translator.translate(&Pg2SqliteOptions::default())?;
    let translated_sql = translated.iter().map(ToString::to_string).collect::<Vec<_>>();

    let update_trigger_sql = translated_sql
        .iter()
        .find(|stmt| stmt.contains("CREATE TRIGGER trigger_upsert_brands_edited_at "))
        .expect("translated BEFORE UPDATE trigger should exist");
    assert!(
        update_trigger_sql.contains("AFTER UPDATE ON brands"),
        "maintenance update branch must be AFTER UPDATE: {update_trigger_sql}"
    );
    assert!(
        update_trigger_sql.contains("WHEN"),
        "maintenance update branch must carry a recursion-guard WHEN clause: {update_trigger_sql}"
    );

    let insert_trigger_sql = translated_sql
        .iter()
        .find(|stmt| {
            stmt.contains("CREATE TRIGGER trigger_upsert_brands_edited_at_pg2sqlite_insert")
        })
        .expect("translated AFTER INSERT trigger should exist");
    assert!(
        insert_trigger_sql.contains("AFTER INSERT ON brands"),
        "maintenance insert branch should be translated to AFTER INSERT: {insert_trigger_sql}"
    );

    let mut connection = SqliteConnection::establish(":memory:")?;
    diesel::sql_query("PRAGMA foreign_keys = ON").execute(&mut connection)?;
    diesel::sql_query("PRAGMA recursive_triggers = ON").execute(&mut connection)?;

    for stmt in translated {
        diesel::sql_query(stmt.to_string()).execute(&mut connection)?;
    }

    diesel::insert_into(brands::table)
        .values(&NewBrand { id: 1, name: "Adidas".to_string(), edited_at: None })
        .execute(&mut connection)?;

    let inserted =
        brands::table.filter(brands::id.eq(1)).select(Brand::as_select()).first(&mut connection)?;
    assert!(
        inserted.edited_at.is_some(),
        "edited_at should be populated after INSERT by the split insert trigger"
    );

    diesel::update(brands::table.filter(brands::id.eq(1)))
        .set(brands::name.eq("Nike"))
        .execute(&mut connection)?;

    diesel::update(brands::table.filter(brands::id.eq(1)))
        .set(brands::edited_at.eq("manual"))
        .execute(&mut connection)?;

    // PostgreSQL fires the trigger even when the caller updates the
    // maintained column directly: the WHEN clause stops the recursion and
    // still lets the trigger run once.
    let updated =
        brands::table.filter(brands::id.eq(1)).select(Brand::as_select()).first(&mut connection)?;
    assert!(
        updated.edited_at.as_deref() != Some("manual"),
        "trigger must fire and override the manual value (PostgreSQL behaviour)"
    );

    Ok(())
}

#[test]
fn test_direct_create_trigger_translation_before_insert_or_update_splits_trigger()
-> Result<(), Box<dyn std::error::Error>> {
    let sql = "
CREATE TABLE brands (
    id SERIAL PRIMARY KEY,
    name VARCHAR(255) NOT NULL,
    edited_at TEXT
);

CREATE OR REPLACE FUNCTION set_brands_edited_at() RETURNS TRIGGER AS $$
BEGIN
    NEW.edited_at = CURRENT_TIMESTAMP;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER trigger_upsert_brands_edited_at
BEFORE INSERT OR UPDATE ON brands
FOR EACH ROW EXECUTE FUNCTION set_brands_edited_at();
";

    let translated = translate_with_direct_create_trigger_path(sql, &Pg2SqliteOptions::default())?;

    let mut connection = SqliteConnection::establish(":memory:")?;
    diesel::sql_query("PRAGMA foreign_keys = ON").execute(&mut connection)?;
    diesel::sql_query("PRAGMA recursive_triggers = ON").execute(&mut connection)?;

    for stmt in translated {
        diesel::sql_query(stmt.to_string()).execute(&mut connection)?;
    }

    let trigger_definitions: Vec<TriggerDefinition> = diesel::sql_query(
        "SELECT name, sql FROM sqlite_master \
         WHERE type = 'trigger' AND name LIKE 'trigger_upsert_brands_edited_at%' ORDER BY name",
    )
    .load(&mut connection)?;

    assert_eq!(
        trigger_definitions.len(),
        2,
        "direct CreateTrigger translation should create distinct INSERT and UPDATE triggers"
    );

    let update_trigger = trigger_definitions
        .iter()
        .find(|trigger| trigger.name == "trigger_upsert_brands_edited_at")
        .expect("translated BEFORE UPDATE trigger should exist");
    assert!(
        update_trigger.sql.contains("AFTER UPDATE ON brands"),
        "maintenance update branch must be AFTER UPDATE: {}",
        update_trigger.sql
    );
    assert!(
        update_trigger.sql.contains("WHEN"),
        "maintenance update branch must carry WHEN clause: {}",
        update_trigger.sql
    );

    let insert_trigger = trigger_definitions
        .iter()
        .find(|trigger| trigger.name == "trigger_upsert_brands_edited_at_pg2sqlite_insert")
        .expect("translated AFTER INSERT trigger should exist");
    assert!(
        insert_trigger.sql.contains("AFTER INSERT ON brands"),
        "maintenance insert branch should be translated to AFTER INSERT: {}",
        insert_trigger.sql
    );

    Ok(())
}

#[test]
fn test_maintenance_trigger_on_rls_table() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
CREATE TABLE brands (
    id SERIAL PRIMARY KEY,
    name VARCHAR(255) NOT NULL,
    edited_at TEXT
);

ALTER TABLE brands ENABLE ROW LEVEL SECURITY;
CREATE POLICY brands_select_all ON brands FOR SELECT USING (true);
CREATE POLICY brands_insert_all ON brands FOR INSERT WITH CHECK (true);
CREATE POLICY brands_update_all ON brands FOR UPDATE USING (true) WITH CHECK (true);

CREATE OR REPLACE FUNCTION update_brands_edited_at() RETURNS TRIGGER AS $$
BEGIN
    NEW.edited_at = CURRENT_TIMESTAMP;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER trigger_update_brands_edited_at
BEFORE UPDATE ON brands
FOR EACH ROW EXECUTE FUNCTION update_brands_edited_at();
";

    let options = Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit");
    let translator = Pg2Sqlite::default().sql(sql)?;
    let translated = translator.translate(&options)?;

    let trigger_sql = translated
        .iter()
        .map(ToString::to_string)
        .find(|sql| sql.contains("CREATE TRIGGER trigger_update_brands_edited_at"))
        .expect("translated trigger statement should exist");
    assert!(
        trigger_sql.contains("AFTER UPDATE ON brands_rls"),
        "maintenance trigger must be AFTER UPDATE on backing table: {trigger_sql}"
    );
    assert!(
        trigger_sql.contains("WHEN"),
        "maintenance trigger must carry a recursion-guard WHEN clause: {trigger_sql}"
    );

    let mut connection = SqliteConnection::establish(":memory:")?;
    diesel::sql_query("PRAGMA foreign_keys = ON").execute(&mut connection)?;
    diesel::sql_query("PRAGMA recursive_triggers = ON").execute(&mut connection)?;

    for stmt in translated {
        diesel::sql_query(stmt.to_string()).execute(&mut connection)?;
    }

    diesel::sql_query(
        "INSERT INTO brands (id, name, edited_at) VALUES (1, 'Adidas', '2020-01-01')",
    )
    .execute(&mut connection)?;

    diesel::sql_query("UPDATE brands SET name = 'Nike' WHERE id = 1").execute(&mut connection)?;

    let updated =
        brands::table.filter(brands::id.eq(1)).select(Brand::as_select()).first(&mut connection)?;
    assert_eq!(updated.name, "Nike");

    Ok(())
}

// ---------------------------------------------------------------------------
// Schema-qualified declarations (R117)
// ---------------------------------------------------------------------------

/// Whether the table was declared as `brands` or `public.brands`, and
/// whichever way the trigger spells it, the emitted script is one and the
/// same. Before the fix a qualified declaration missed the maintenance path
/// and fell to the plpgsql refusal for `NEW` assignments.
#[test]
fn all_four_qualification_combinations_emit_the_same_script()
-> Result<(), Box<dyn std::error::Error>> {
    let spell = |decl: &str, on: &str| {
        format!(
            "CREATE TABLE {decl} (
    id SERIAL PRIMARY KEY,
    name VARCHAR(255) NOT NULL,
    edited_at TEXT
);

CREATE OR REPLACE FUNCTION update_brands_edited_at() RETURNS TRIGGER AS $$
BEGIN
    NEW.edited_at = CURRENT_TIMESTAMP;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER trigger_update_brands_edited_at
BEFORE UPDATE ON {on}
FOR EACH ROW EXECUTE FUNCTION update_brands_edited_at();"
        )
    };

    let baseline = Pg2Sqlite::default()
        .sql(&spell("brands", "brands"))?
        .translate_to_sql(&Pg2SqliteOptions::default())?;

    for (decl, on) in [
        ("brands", "public.brands"),
        ("public.brands", "brands"),
        ("public.brands", "public.brands"),
    ] {
        let translated = Pg2Sqlite::default()
            .sql(&spell(decl, on))?
            .translate_to_sql(&Pg2SqliteOptions::default())?;
        assert_eq!(
            translated, baseline,
            "declaring {decl} and triggering on {on} must not change the emitted script"
        );
    }

    // The shared script runs and the trigger stamps the row.
    let mut connection = SqliteConnection::establish(":memory:")?;
    for stmt in &baseline {
        diesel::sql_query(stmt.clone()).execute(&mut connection)?;
    }
    diesel::insert_into(brands::table)
        .values(&NewBrand {
            id: 1,
            name: "Adidas".to_string(),
            edited_at: Some("2020-01-01".to_string()),
        })
        .execute(&mut connection)?;
    diesel::update(brands::table.filter(brands::id.eq(1)))
        .set(brands::name.eq("Nike"))
        .execute(&mut connection)?;
    let count = diesel::update(
        brands::table.filter(brands::id.eq(1).and(brands::edited_at.ne("2020-01-01"))),
    )
    .set(brands::name.eq("Verified"))
    .execute(&mut connection)?;
    assert_eq!(count, 1, "the maintenance trigger must have stamped edited_at");

    Ok(())
}

/// The RLS redirect engages for a declaration it never saw before the fix:
/// the whole script is qualified the way pg_dump spells it, and the trigger
/// must land on the backing table all the same. A mixed spelling, declaring
/// `public.brands` and altering `brands`, is refused earlier by the schema
/// build itself, so the uniform shape is the one this pins.
#[test]
fn a_qualified_rls_declaration_still_lands_on_the_backing_table()
-> Result<(), Box<dyn std::error::Error>> {
    let sql = "
CREATE TABLE public.brands (
    id SERIAL PRIMARY KEY,
    name VARCHAR(255) NOT NULL,
    edited_at TEXT
);

ALTER TABLE public.brands ENABLE ROW LEVEL SECURITY;
CREATE POLICY brands_select_all ON public.brands FOR SELECT USING (true);
CREATE POLICY brands_insert_all ON public.brands FOR INSERT WITH CHECK (true);
CREATE POLICY brands_update_all ON public.brands FOR UPDATE USING (true) WITH CHECK (true);

CREATE OR REPLACE FUNCTION update_brands_edited_at() RETURNS TRIGGER AS $$
BEGIN
    NEW.edited_at = CURRENT_TIMESTAMP;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER trigger_update_brands_edited_at
BEFORE UPDATE ON public.brands
FOR EACH ROW EXECUTE FUNCTION update_brands_edited_at();
";

    let options = Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit");
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let trigger_sql = translated
        .iter()
        .map(ToString::to_string)
        .find(|sql| sql.contains("CREATE TRIGGER trigger_update_brands_edited_at"))
        .expect("translated trigger statement should exist");
    assert!(
        trigger_sql.contains("AFTER UPDATE ON brands_rls"),
        "maintenance trigger must target the backing table as AFTER UPDATE: {trigger_sql}"
    );
    assert!(
        trigger_sql.contains("WHEN"),
        "maintenance trigger must carry a WHEN clause: {trigger_sql}"
    );

    let mut connection = SqliteConnection::establish(":memory:")?;
    diesel::sql_query("PRAGMA foreign_keys = ON").execute(&mut connection)?;
    diesel::sql_query("PRAGMA recursive_triggers = ON").execute(&mut connection)?;
    for stmt in translated {
        diesel::sql_query(stmt.to_string()).execute(&mut connection)?;
    }
    diesel::sql_query(
        "INSERT INTO brands (id, name, edited_at) VALUES (1, 'Adidas', '2020-01-01')",
    )
    .execute(&mut connection)?;
    diesel::sql_query("UPDATE brands SET name = 'Nike' WHERE id = 1").execute(&mut connection)?;

    let updated =
        brands::table.filter(brands::id.eq(1)).select(Brand::as_select()).first(&mut connection)?;
    assert_eq!(updated.name, "Nike");

    Ok(())
}

// ── Two-column maintenance trigger → OR condition in WHEN clause ─────────────

/// A maintenance trigger that assigns to two columns emits a WHEN clause with
/// two IS DISTINCT FROM conditions joined by OR (create_trigger.rs 318-320).
/// One update changes both columns; recursion must not fire a second time.
#[test]
fn maintenance_trigger_two_columns_when_clause_uses_or() {
    let sql = "
CREATE TABLE widgets (id INT PRIMARY KEY, tag TEXT, slug TEXT);

CREATE OR REPLACE FUNCTION widgets_maintain() RETURNS TRIGGER AS $$
BEGIN
    NEW.tag := NEW.tag || '!';
    NEW.slug := upper(NEW.slug);
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER widgets_maint BEFORE UPDATE ON widgets
FOR EACH ROW EXECUTE FUNCTION widgets_maintain();
";
    let stmts = Pg2Sqlite::default()
        .sql(sql)
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("translate");

    let trigger_sql = stmts
        .iter()
        .map(ToString::to_string)
        .find(|s| s.contains("AFTER UPDATE"))
        .expect("maintenance trigger must be in output");
    assert!(
        trigger_sql.matches("IS DISTINCT FROM").count() >= 2,
        "two maintained columns must produce two IS DISTINCT FROM clauses: {trigger_sql}"
    );
    assert!(trigger_sql.contains(" OR "), "two conditions must be joined by OR: {trigger_sql}");

    let mut conn = SqliteConnection::establish(":memory:").expect("connect");
    diesel::sql_query("PRAGMA recursive_triggers = ON").execute(&mut conn).expect("pragma");
    for stmt in &stmts {
        diesel::sql_query(stmt.as_str())
            .execute(&mut conn)
            .unwrap_or_else(|e| panic!("DDL: {e}\n{stmt}"));
    }
    diesel::insert_into(widgets::table)
        .values(&NewWidget { id: 1, tag: Some("hello".into()), slug: Some("world".into()) })
        .execute(&mut conn)
        .expect("insert");
    // Trigger fires on UPDATE: tag gets '!', slug gets uppercased.
    diesel::update(widgets::table.filter(widgets::id.eq(1)))
        .set((widgets::tag.eq("hello"), widgets::slug.eq("world")))
        .execute(&mut conn)
        .expect("update");
    let w = widgets::table
        .filter(widgets::id.eq(1))
        .select(Widget::as_select())
        .first(&mut conn)
        .expect("select");
    assert_eq!(w.tag.as_deref(), Some("hello!"), "tag must have '!' appended by maintenance");
    assert_eq!(w.slug.as_deref(), Some("WORLD"), "slug must be uppercased by maintenance");
}

// ── Maintenance trigger with a source WHEN clause → AND merge ────────────────

/// When the source trigger already has a WHEN clause, the recursion guard
/// appended by the translation is ANDed with it (create_trigger.rs 327-332,
/// line 657).  The merged clause must contain both predicates.
#[test]
fn maintenance_trigger_with_source_when_clause_merges_recursion_guard() {
    let sql = "
CREATE TABLE w2 (id INT PRIMARY KEY, tag TEXT);

CREATE OR REPLACE FUNCTION w2_maintain() RETURNS TRIGGER AS $$
BEGIN
    NEW.tag := NEW.tag || '!';
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER w2_maint BEFORE UPDATE ON w2
FOR EACH ROW WHEN (OLD.id > 0) EXECUTE FUNCTION w2_maintain();
";
    let stmts = Pg2Sqlite::default()
        .sql(sql)
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("translate");

    let trigger_sql = stmts
        .iter()
        .map(ToString::to_string)
        .find(|s| s.contains("AFTER UPDATE"))
        .expect("maintenance trigger must be in output");
    // Source WHEN (OLD.id > 0) merged with recursion guard using AND.
    assert!(trigger_sql.contains(" AND "), "merged WHEN clause must use AND: {trigger_sql}");
    assert!(
        trigger_sql.contains("OLD.id"),
        "source WHEN predicate must survive the merge: {trigger_sql}"
    );
    assert!(
        trigger_sql.contains("IS DISTINCT FROM"),
        "recursion guard must be present: {trigger_sql}"
    );
    // Execute: trigger fires only when OLD.id > 0, which is always true here.
    let mut conn = SqliteConnection::establish(":memory:").expect("connect");
    diesel::sql_query("PRAGMA recursive_triggers = ON").execute(&mut conn).expect("pragma");
    for stmt in &stmts {
        diesel::sql_query(stmt.as_str())
            .execute(&mut conn)
            .unwrap_or_else(|e| panic!("DDL: {e}\n{stmt}"));
    }
    // Typed Diesel DSL for DML; translator-emitted DDL above uses sql_query
    // (dynamic schema).
    diesel::insert_into(w2::table)
        .values(&NewW2 { id: 1, tag: Some("hi".into()) })
        .execute(&mut conn)
        .expect("insert");
    diesel::update(w2::table.filter(w2::id.eq(1)))
        .set(w2::tag.eq("hi"))
        .execute(&mut conn)
        .expect("update");
    // After UPDATE the maintenance trigger fires and appends '!'.
    let row =
        w2::table.filter(w2::id.eq(1)).select(W2Row::as_select()).first(&mut conn).expect("select");
    assert_eq!(row.tag.as_deref(), Some("hi!"), "maintenance trigger must have appended '!'");
}
