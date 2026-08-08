//! PostgreSQL's two UUID generators reach two different destination
//! functions, and the time-ordered one is refused until the caller names it.
//!
//! `gen_random_uuid()`, `uuid_generate_v4()` and `uuidv4()` make a random
//! version 4 UUID. `uuidv7()` makes a version 7 one, whose first 48 bits are
//! the millisecond it was created, which is what schemas leaning on sortable
//! identifiers and index locality are buying.
//!
//! Both used to collapse onto the single `with_uuid_function_name` value, so
//! the translation was wrong in whichever direction that name pointed: left at
//! its `uuid` default a `uuidv7()` came back random, and set to a version 7
//! function a `gen_random_uuid()` came back carrying its own creation time.
//!
//! The destination's spelling is its own: SQLite's bundled `uuid.c` registers
//! `uuid()` and has no version 7 at all, and SQLean's `uuid` module calls its
//! version 7 generator `uuid7()`. So the name is configured rather than
//! assumed.

mod helpers;

use diesel::{QueryableByName, RunQueryDsl, connection::SimpleConnection, sql_query};
use helpers::{establish_connection, translate_pg};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, TranslationOptions, UuidRepresentation};

/// The name the test harness registers a real version 7 generator under.
const V7: &str = "uuidv7";

fn without_v7() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default().with_uuid_representation(UuidRepresentation::Blob)
}

fn with_v7() -> Pg2SqliteOptions {
    without_v7().with_uuid_v7_function_name(V7)
}

fn translate(pg: &str, options: &Pg2SqliteOptions) -> String {
    translate_pg(pg, options).expect("translate").join("\n")
}

fn refusal(pg: &str, options: &Pg2SqliteOptions) -> String {
    translate_pg(pg, options).expect_err("must be refused").to_string()
}

// ---------- the refusal ----------

#[test]
fn uuidv7_is_refused_when_no_v7_function_is_declared() {
    let message = refusal("SELECT uuidv7();", &without_v7());
    assert!(message.contains("uuidv7"), "must name the function: {message}");
    assert!(message.contains("with_uuid_v7_function_name"), "must name the remedy: {message}");
}

#[test]
fn a_column_default_of_uuidv7_is_refused_when_no_v7_function_is_declared() {
    let message = refusal("CREATE TABLE a (id UUID PRIMARY KEY DEFAULT uuidv7());", &without_v7());
    assert!(message.contains("uuidv7"), "must name the function: {message}");
}

#[test]
fn a_plpgsql_body_calling_uuidv7_is_refused_when_no_v7_function_is_declared() {
    let message = refusal(
        "CREATE TABLE ev (id INT PRIMARY KEY);
         CREATE TABLE au (id UUID);
         CREATE FUNCTION f() RETURNS trigger LANGUAGE plpgsql AS $$
         DECLARE v_id UUID := uuidv7();
         BEGIN
           INSERT INTO au (id) VALUES (v_id);
           RETURN NEW;
         END; $$;
         CREATE TRIGGER tr AFTER INSERT ON ev FOR EACH ROW EXECUTE FUNCTION f();",
        &without_v7(),
    );
    assert!(message.contains("uuidv7"), "must name the function: {message}");
}

// ---------- the two generators stay apart ----------

#[test]
fn the_declared_v7_function_is_what_uuidv7_emits() {
    let emitted = translate("SELECT uuidv7();", &with_v7());
    assert_eq!(emitted, format!("SELECT {V7}()"));
}

/// The mirror-image defect. Naming a version 7 function must not change what a
/// deliberately random generator emits, or a schema that asked for an opaque
/// identifier silently starts publishing when each row was made.
#[test]
fn the_random_generators_never_emit_the_v7_function() {
    for spelling in ["gen_random_uuid", "uuid_generate_v4", "uuidv4"] {
        let emitted = translate(&format!("SELECT {spelling}();"), &with_v7());
        assert_eq!(emitted, "SELECT uuid()", "{spelling}");
    }
}

/// The two names are independent, so a caller can point each at its own
/// destination function.
#[test]
fn the_two_generator_names_are_configured_separately() {
    let options = without_v7().with_uuid_function_name("uuid4").with_uuid_v7_function_name("uuid7");
    assert_eq!(translate("SELECT gen_random_uuid();", &options), "SELECT uuid4()");
    assert_eq!(translate("SELECT uuidv7();", &options), "SELECT uuid7()");
}

#[test]
fn a_column_default_of_uuidv7_emits_the_declared_v7_function() {
    let emitted = translate("CREATE TABLE a (id UUID PRIMARY KEY DEFAULT uuidv7());", &with_v7());
    assert!(emitted.contains(&format!("DEFAULT ({V7}())")), "{emitted}");
}

#[test]
fn a_plpgsql_body_calling_uuidv7_emits_the_declared_v7_function() {
    let emitted = translate(
        "CREATE TABLE ev (id INT PRIMARY KEY);
         CREATE TABLE au (id UUID);
         CREATE FUNCTION f() RETURNS trigger LANGUAGE plpgsql AS $$
         DECLARE v_id UUID := uuidv7();
         BEGIN
           INSERT INTO au (id) VALUES (v_id);
           RETURN NEW;
         END; $$;
         CREATE TRIGGER tr AFTER INSERT ON ev FOR EACH ROW EXECUTE FUNCTION f();",
        &with_v7(),
    );
    assert!(emitted.contains(&format!("{V7}()")), "{emitted}");
    assert!(!emitted.contains("uuid()"), "must not fall back to the random generator: {emitted}");
}

/// A call written directly against the declared name is a call to a function
/// the destination was just said to have, so it passes through rather than
/// being refused as unrecognised.
#[test]
fn a_direct_call_to_the_declared_v7_name_passes_through() {
    let options = without_v7().with_uuid_v7_function_name("uuid7");
    assert_eq!(translate("SELECT uuid7();", &options), "SELECT uuid7()");
}

// ---------- what actually lands in the column ----------

#[derive(QueryableByName)]
struct StoredId {
    #[diesel(sql_type = diesel::sql_types::Binary)]
    id: Vec<u8>,
}

/// The point of the whole item: the value stored by the emitted default
/// carries its own creation time, and a row made later sorts after one made
/// earlier.
///
/// Per RFC 9562 a version 7 UUID is the creation time in milliseconds since
/// the Unix epoch in its first six bytes, big endian, then the version nibble
/// in the high half of byte 6 and the variant in the top two bits of byte 8.
/// The timestamp assertion is what discriminates: a version 4 value, which is
/// what this used to store, has random bytes there and would land millions of
/// years away.
///
/// The two rows are made two milliseconds apart, because without the optional
/// monotonic counter two version 7 values from the same millisecond share a
/// timestamp and differ only in random bits, so their order within a
/// millisecond is not a property to assert.
#[test]
fn the_stored_default_is_a_version_7_uuid() {
    let mut connection = establish_connection();
    let ddl = translate(
        "CREATE TABLE t (id UUID PRIMARY KEY DEFAULT uuidv7(), label TEXT NOT NULL);",
        &with_v7(),
    );
    connection.batch_execute(&format!("{ddl};")).expect("apply the emitted DDL");

    let before = unix_milliseconds();
    connection.batch_execute("INSERT INTO t (label) VALUES ('a');").expect("insert the first row");
    std::thread::sleep(std::time::Duration::from_millis(2));
    connection.batch_execute("INSERT INTO t (label) VALUES ('b');").expect("insert the second row");
    let after = unix_milliseconds();

    let rows: Vec<StoredId> = sql_query("SELECT id FROM t ORDER BY label")
        .load(&mut connection)
        .expect("read the identifiers back");
    assert_eq!(rows.len(), 2);
    for row in &rows {
        assert_eq!(row.id.len(), 16, "a UUID is sixteen bytes");
        assert_eq!(row.id[6] >> 4, 7, "version nibble must be 7, got {:?}", row.id);
        assert_eq!(row.id[8] & 0xc0, 0x80, "variant bits must be RFC 9562, got {:?}", row.id);
        let stamp = embedded_milliseconds(&row.id);
        assert!(
            (before..=after).contains(&stamp),
            "the embedded time {stamp} must fall between {before} and {after}"
        );
    }
    assert!(
        embedded_milliseconds(&rows[0].id) < embedded_milliseconds(&rows[1].id),
        "the later row must carry the later time"
    );
    assert!(rows[0].id < rows[1].id, "and therefore the larger identifier");
}

/// Now, in milliseconds since the Unix epoch.
fn unix_milliseconds() -> u64 {
    u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("a clock after 1970")
            .as_millis(),
    )
    .expect("a millisecond count inside u64")
}

/// The 48-bit big-endian timestamp a version 7 UUID opens with.
fn embedded_milliseconds(id: &[u8]) -> u64 {
    id[..6].iter().fold(0, |stamp, byte| (stamp << 8) | u64::from(*byte))
}

// ---------- the way back ----------

#[test]
fn the_declared_v7_function_reverses_to_uuidv7() {
    let options = without_v7().with_uuid_v7_function_name("uuid7");
    let translator = Pg2Sqlite::default().sql("CREATE TABLE t (id INT);").expect("parse");
    let schema = translator.build_schema().expect("build the schema");
    let reversed =
        translator.reverse_sql("SELECT uuid7();", &schema, &options).expect("reverse translate");
    assert!(reversed.last().expect("a statement").to_string().contains("uuidv7()"));
}

#[test]
fn the_random_function_still_reverses_to_gen_random_uuid() {
    let options = without_v7().with_uuid_v7_function_name("uuid7");
    let translator = Pg2Sqlite::default().sql("CREATE TABLE t (id INT);").expect("parse");
    let schema = translator.build_schema().expect("build the schema");
    let reversed =
        translator.reverse_sql("SELECT uuid();", &schema, &options).expect("reverse translate");
    assert!(reversed.last().expect("a statement").to_string().contains("gen_random_uuid()"));
}
