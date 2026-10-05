//! Round-two PL/pgSQL findings: R2-4 panic, T-A recheck, R2-5 TG_OP,
//! R2-13 variable in UPDATE/DELETE, R2-14 RAISE USING, R2-15 RETURN OLD,
//! and the WHILE/CASE message wording.

#![allow(missing_docs)]

mod helpers;
use diesel::{RunQueryDsl, prelude::*};
use helpers::establish_connection;
use pg2sqlite::prelude::Pg2SqliteOptions;

fn translate(sql: &str) -> Result<Vec<String>, pg2sqlite::errors::Error> {
    helpers::translate_pg(sql, &Pg2SqliteOptions::default())
}

fn apply_all(conn: &mut diesel::SqliteConnection, stmts: &[String]) {
    for s in stmts {
        diesel::sql_query(s.as_str())
            .execute(conn)
            .unwrap_or_else(|e| panic!("translated DDL/DML failed: {e}\n{s}"));
    }
}

// ── R2-4: non-ASCII char in trigger body must never panic
// ──────────────────────

#[test]
fn r2_4_non_ascii_in_trigger_body_does_not_panic() {
    // ILIKE 'CAFÉ%' positions É at a byte boundary that used to cause
    // preprocessor.rs:711 to panic with "byte index is not a char boundary".
    let sql = "
        CREATE TABLE r2_4_t (id INT PRIMARY KEY, name TEXT);
        CREATE FUNCTION r2_4_f() RETURNS TRIGGER LANGUAGE plpgsql AS $$
        BEGIN
            IF NEW.name ILIKE 'CAF\u{c9}%' THEN
                UPDATE r2_4_t SET name = 'found' WHERE id = NEW.id;
            END IF;
            RETURN NEW;
        END;
        $$;
        CREATE TRIGGER r2_4_trg
        BEFORE INSERT ON r2_4_t
        FOR EACH ROW EXECUTE FUNCTION r2_4_f();
    ";
    // Must return Ok or Err; a panic aborts the test process.
    let result = translate(sql);
    // Confirm result is inspectable (not a panic):
    let _ = format!("{result:?}");
}

// ── R2-4 T-A: untranslatable IF condition must propagate an error
// ──────────────

#[test]
fn r2_4_ta_untranslatable_if_condition_is_refused() {
    // SIMILAR TO has no SQLite equivalent. With the old map_or_else swallow,
    // the raw SIMILAR TO text would be injected into the WHERE clause and the
    // trigger would fail at fire time rather than translation time.
    let sql = "
        CREATE TABLE r2_4ta_t (id INT PRIMARY KEY, name TEXT);
        CREATE TABLE r2_4ta_log (note TEXT);
        CREATE FUNCTION r2_4ta_f() RETURNS TRIGGER LANGUAGE plpgsql AS $$
        BEGIN
            IF NEW.name SIMILAR TO '[A-Z]%' THEN
                INSERT INTO r2_4ta_log (note) VALUES ('matched');
            END IF;
            RETURN NEW;
        END;
        $$;
        CREATE TRIGGER r2_4ta_trg
        AFTER INSERT ON r2_4ta_t
        FOR EACH ROW EXECUTE FUNCTION r2_4ta_f();
    ";
    // After the T-A fix, untranslatable conditions must produce an Err at
    // translation time, not silently pass through to fire-time failure.
    let result = translate(sql);
    assert!(
        result.is_err(),
        "an untranslatable IF condition must be refused at translation time, \
         not silently injected as raw PostgreSQL text: {result:?}"
    );
}

// ── R2-5: TG_OP constant-folded per emitted trigger event
// ─────────────────────

diesel::table! {
    r2_5_items (id) {
        id -> Integer,
    }
}

diesel::table! {
    r2_5_audit (id) {
        id -> Integer,
        op -> Text,
    }
}

#[test]
fn r2_5_tg_op_is_constant_folded_in_insert_trigger() {
    let sql = "
        CREATE TABLE r2_5_items (id INT PRIMARY KEY);
        CREATE TABLE r2_5_audit (id INTEGER PRIMARY KEY AUTOINCREMENT, op TEXT NOT NULL);

        CREATE FUNCTION r2_5_f() RETURNS TRIGGER LANGUAGE plpgsql AS $$
        BEGIN
            IF TG_OP = 'INSERT' THEN
                INSERT INTO r2_5_audit (op) VALUES ('INSERT');
            END IF;
            RETURN NEW;
        END;
        $$;

        CREATE TRIGGER r2_5_trg
        AFTER INSERT ON r2_5_items
        FOR EACH ROW EXECUTE FUNCTION r2_5_f();
    ";

    let stmts = translate(sql).expect("trigger with TG_OP must translate");
    let mut conn = establish_connection();
    apply_all(&mut conn, &stmts);

    // Fire the trigger by inserting a row.
    diesel::sql_query("INSERT INTO r2_5_items (id) VALUES (1)")
        .execute(&mut conn)
        .expect("insert must succeed");

    // The audit row must land; without TG_OP constant-folding this fails with
    // "no such column: TG_OP".
    let count: i64 = r2_5_audit::table.count().get_result(&mut conn).unwrap();
    assert_eq!(count, 1, "audit row must land when TG_OP = 'INSERT' folds to TRUE");
}

// ── R2-5 companion: multi-event TG_OP trigger — pin current contract
// ──────────

#[test]
fn r2_5_multi_event_tg_op_trigger_current_contract() {
    // An INSERT OR UPDATE trigger whose body branches on TG_OP. Measure what
    // the translator does today and pin it. If the translator refuses, that IS
    // the contract; if it translates, record the fire-time behavior.
    let sql = "
        CREATE TABLE r2_5_multi_t (id INT PRIMARY KEY, val INT);
        CREATE TABLE r2_5_multi_log (op TEXT);

        CREATE FUNCTION r2_5_multi_f() RETURNS TRIGGER LANGUAGE plpgsql AS $$
        BEGIN
            IF TG_OP = 'INSERT' THEN
                INSERT INTO r2_5_multi_log (op) VALUES ('INSERT');
            ELSIF TG_OP = 'UPDATE' THEN
                INSERT INTO r2_5_multi_log (op) VALUES ('UPDATE');
            END IF;
            RETURN NEW;
        END;
        $$;

        CREATE TRIGGER r2_5_multi_trg
        AFTER INSERT OR UPDATE ON r2_5_multi_t
        FOR EACH ROW EXECUTE FUNCTION r2_5_multi_f();
    ";

    let result = translate(sql);
    // Pin: multi-event TG_OP triggers are refused at translation time because
    // TG_OP is ambiguous without per-event trigger splitting.
    // If this unexpectedly passes, report the measured fire-time behavior.
    assert!(
        result.is_err(),
        "multi-event TG_OP trigger is expected to be refused (ambiguous TG_OP); \
         if it translates now, update this pin with the measured fire-time behavior"
    );
}

// ── R2-13: plpgsql variable referenced in UPDATE/DELETE path ─────────────────

diesel::table! {
    r2_13_source (id) {
        id -> Integer,
    }
}

diesel::table! {
    r2_13_target (id) {
        id -> Integer,
    }
}

diesel::table! {
    r2_13_memo (id) {
        id -> Integer,
        note -> Text,
    }
}

#[test]
fn r2_13_variable_in_if_condition_guards_update() {
    let sql = "
        CREATE TABLE r2_13_source (id INT PRIMARY KEY);
        CREATE TABLE r2_13_target (id INT PRIMARY KEY);
        CREATE TABLE r2_13_memo (id INT PRIMARY KEY, note TEXT NOT NULL);

        CREATE FUNCTION r2_13_f() RETURNS TRIGGER LANGUAGE plpgsql AS $$
        DECLARE
            v_cnt INTEGER;
        BEGIN
            SELECT count(*) INTO v_cnt FROM r2_13_source;
            IF v_cnt > 0 THEN
                UPDATE r2_13_memo SET note = 'updated' WHERE id = 1;
            END IF;
            RETURN NEW;
        END;
        $$;

        CREATE TRIGGER r2_13_trg
        AFTER INSERT ON r2_13_target
        FOR EACH ROW EXECUTE FUNCTION r2_13_f();
    ";

    let stmts = translate(sql).expect("trigger with SELECT INTO + IF UPDATE must translate");
    let mut conn = establish_connection();
    apply_all(&mut conn, &stmts);

    // Pre-populate: source has rows, memo starts as 'initial'.
    diesel::sql_query("INSERT INTO r2_13_source (id) VALUES (1), (2), (3)")
        .execute(&mut conn)
        .unwrap();
    diesel::sql_query("INSERT INTO r2_13_memo (id, note) VALUES (1, 'initial')")
        .execute(&mut conn)
        .unwrap();

    // Fire the trigger.
    diesel::sql_query("INSERT INTO r2_13_target (id) VALUES (42)")
        .execute(&mut conn)
        .expect("insert that fires trigger must succeed");

    // Without variable substitution in the condition, the UPDATE fires with
    // WHERE (v_cnt > 0) which fails with "no such column: v_cnt".
    let note: String = r2_13_memo::table
        .select(r2_13_memo::note)
        .filter(r2_13_memo::id.eq(1))
        .get_result(&mut conn)
        .unwrap();
    assert_eq!(note, "updated", "UPDATE guarded by v_cnt must apply when source has rows");
}

// ── R2-14: RAISE EXCEPTION USING MESSAGE = '<literal>' ───────────────────────

diesel::table! {
    r2_14_t (id) {
        id -> Integer,
        val -> Integer,
    }
}

#[test]
fn r2_14_raise_exception_using_message_literal_translates_and_fires() {
    let sql = "
        CREATE TABLE r2_14_t (id INT PRIMARY KEY, val INT NOT NULL);

        CREATE FUNCTION r2_14_f() RETURNS TRIGGER LANGUAGE plpgsql AS $$
        BEGIN
            IF NEW.val < 0 THEN
                RAISE EXCEPTION USING MESSAGE = 'negative value not allowed';
            END IF;
            RETURN NEW;
        END;
        $$;

        CREATE TRIGGER r2_14_trg
        BEFORE INSERT ON r2_14_t
        FOR EACH ROW EXECUTE FUNCTION r2_14_f();
    ";

    // Currently fails: RAISE EXCEPTION USING emits invalid SQL and
    // re-parse fails with a generic message naming nothing.
    let stmts = translate(sql).expect("RAISE EXCEPTION USING MESSAGE = '<literal>' must translate");

    let mut conn = establish_connection();
    apply_all(&mut conn, &stmts);

    // A valid insert must succeed.
    diesel::sql_query("INSERT INTO r2_14_t (id, val) VALUES (1, 5)")
        .execute(&mut conn)
        .expect("valid insert must succeed");

    // An insert violating the condition must raise 'negative value not
    // allowed'.
    let result =
        diesel::sql_query("INSERT INTO r2_14_t (id, val) VALUES (2, -1)").execute(&mut conn);
    let err = result.expect_err("insert must be refused by trigger").to_string();
    assert!(
        err.contains("negative value not allowed"),
        "RAISE message must surface in the error, got: {err}"
    );
}

// ── R2-15: RETURN OLD treated as no-op like RETURN NEW ───────────────────────

diesel::table! {
    r2_15_t (id) {
        id -> Integer,
        val -> Integer,
    }
}

#[test]
fn r2_15_return_old_in_trigger_body_translates_and_fires() {
    let sql = "
        CREATE TABLE r2_15_t (id INT PRIMARY KEY, val INT NOT NULL);

        CREATE FUNCTION r2_15_f() RETURNS TRIGGER LANGUAGE plpgsql AS $$
        BEGIN
            IF NEW.val >= 0 THEN
                RETURN NEW;
            ELSE
                RETURN OLD;
            END IF;
        END;
        $$;

        CREATE TRIGGER r2_15_trg
        BEFORE INSERT ON r2_15_t
        FOR EACH ROW EXECUTE FUNCTION r2_15_f();
    ";

    // Currently fails: RETURN OLD is refused with
    // "RETURN OLD has no SQLite equivalent in a trigger body".
    let stmts = translate(sql).expect("RETURN OLD must translate as a no-op like RETURN NEW");

    let mut conn = establish_connection();
    apply_all(&mut conn, &stmts);

    // Positive val: RETURN NEW path fires, row lands.
    diesel::insert_into(r2_15_t::table)
        .values((r2_15_t::id.eq(1), r2_15_t::val.eq(5)))
        .execute(&mut conn)
        .expect("row with positive val must land");

    // Negative val: RETURN OLD path fires (no-op), row still lands.
    diesel::insert_into(r2_15_t::table)
        .values((r2_15_t::id.eq(2), r2_15_t::val.eq(-3)))
        .execute(&mut conn)
        .expect("row with negative val must also land");

    let count: i64 = r2_15_t::table.count().get_result(&mut conn).unwrap();
    assert_eq!(count, 2, "both rows must land");
}

// ── WHILE/CASE: message must not claim 'outside a trigger body'
// ───────────────

#[test]
fn while_in_trigger_body_message_names_plpgsql_limitation() {
    let sql = "
        CREATE TABLE while_t (id INT PRIMARY KEY, val INT);
        CREATE FUNCTION while_f() RETURNS TRIGGER LANGUAGE plpgsql AS $$
        DECLARE
            i INT := 0;
        BEGIN
            WHILE i < NEW.val LOOP
                i := i + 1;
            END LOOP;
            RETURN NEW;
        END;
        $$;
        CREATE TRIGGER while_trg
        AFTER INSERT ON while_t
        FOR EACH ROW EXECUTE FUNCTION while_f();
    ";

    let err = translate(sql).expect_err("WHILE inside trigger body must be refused").to_string();

    assert!(
        !err.contains("outside a trigger body"),
        "message must not falsely claim the loop is 'outside a trigger body'; got: {err}"
    );
    // The message must name what the limitation is (loop/procedural control).
    assert!(
        err.to_lowercase().contains("loop")
            || err.to_lowercase().contains("while")
            || err.to_lowercase().contains("procedural"),
        "message must name the limitation (loop/while/procedural), got: {err}"
    );
}
