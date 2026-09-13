//! Bind-parameter contract: a caller binds what PostgreSQL takes.
//!
//! The emitted SQL performs every re-representation conversion so the stored
//! value and the bound value are always in the same terms.

use pg2sqlite::prelude::{ArrayRepresentation, Pg2Sqlite, Pg2SqliteOptions};

// ── NUMERIC(p,s)
// ──────────────────────────────────────────────────────────────

mod numeric {
    use diesel::{RunQueryDsl, prelude::*, sql_query, sqlite::SqliteConnection};
    use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

    diesel::table! {
        t (id) {
            id -> Integer,
            amount -> BigInt,
        }
    }

    fn stmts(pg: &str) -> Vec<String> {
        Pg2Sqlite::default()
            .sql(pg)
            .expect("parse")
            .translate_to_sql(&Pg2SqliteOptions::default())
            .expect("translate")
    }

    #[derive(QueryableByName, Debug, PartialEq)]
    struct IdRow {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        id: i32,
    }

    /// Binding 2.00 against a NUMERIC(10,2) column holding 1.50 finds no rows.
    /// Before the fix minor-unit storage (150) was compared against the raw
    /// float, and 150 > 2.0 incorrectly returned the row.
    #[test]
    fn param_select_uses_postgres_scale() {
        let all = stmts(
            "CREATE TABLE t (id INT PRIMARY KEY, amount NUMERIC(10,2));
             INSERT INTO t (id, amount) VALUES (1, 1.50);
             SELECT id FROM t WHERE amount > $1;",
        );
        let mut conn = SqliteConnection::establish(":memory:").expect("open");
        // STRICT and CHECK constraints cannot be expressed in diesel's typed
        // DSL.
        sql_query(&all[0]).execute(&mut conn).expect("DDL");
        sql_query(&all[1]).execute(&mut conn).expect("seed");

        // Translator emits the exact SELECT; typed DSL cannot reproduce
        // placeholder wrapping.
        let probe = all
            .iter()
            .find(|s| s.to_ascii_uppercase().trim_start().starts_with("SELECT"))
            .expect("SELECT");

        let over_two: Vec<IdRow> = sql_query(probe)
            .bind::<diesel::sql_types::Double, _>(2.0_f64)
            .load(&mut conn)
            .expect("probe 2.0");
        assert_eq!(over_two, vec![], "1.50 must not be greater than 2.00");

        let over_one: Vec<IdRow> = sql_query(probe)
            .bind::<diesel::sql_types::Double, _>(1.0_f64)
            .load(&mut conn)
            .expect("probe 1.0");
        assert_eq!(over_one, vec![IdRow { id: 1 }], "1.50 must be greater than 1.00");
    }

    /// A bound decimal INSERT stores minor units; typed DSL reads them back.
    #[test]
    fn param_insert_stores_minor_units() {
        let all = stmts(
            "CREATE TABLE t (id INT PRIMARY KEY, amount NUMERIC(10,2));
             INSERT INTO t (id, amount) VALUES (1, $1);",
        );
        let mut conn = SqliteConnection::establish(":memory:").expect("open");
        sql_query(&all[0]).execute(&mut conn).expect("DDL");
        // Translator-emitted INSERT tested; typed DSL cannot reproduce the
        // placeholder wrap.
        sql_query(&all[1])
            .bind::<diesel::sql_types::Double, _>(1.5_f64)
            .execute(&mut conn)
            .expect("parameterised INSERT must succeed");

        let stored: i64 =
            t::table.filter(t::id.eq(1_i32)).select(t::amount).first(&mut conn).expect("read");
        assert_eq!(stored, 150_i64, "1.50 must be stored as 150 minor units");
    }

    /// The manifest publishes the scale so consumers can recover the original
    /// value. Binding 1.50 stores 150; dividing by 10^2 recovers the
    /// integer parts.
    #[test]
    fn manifest_scale_round_trips_bound_value() {
        let opts = Pg2SqliteOptions::default();
        let manifest = Pg2Sqlite::default()
            .sql("CREATE TABLE t (id INT PRIMARY KEY, amount NUMERIC(10,2));")
            .expect("parse")
            .translation_manifest(&opts)
            .expect("manifest");

        let entry = manifest.iter().find(|e| e.logical == "t").expect("table t");
        let col = entry.columns.iter().find(|c| c.name == "amount").expect("amount");
        let pg2sqlite::manifest::ColumnStorage::MinorUnits { scale } = col.storage else {
            panic!("a NUMERIC column must publish its scale, got {:?}", col.storage);
        };
        assert_eq!(scale, 2_u32);

        let all = stmts(
            "CREATE TABLE t (id INT PRIMARY KEY, amount NUMERIC(10,2));
             INSERT INTO t (id, amount) VALUES (1, $1);",
        );
        let mut conn = SqliteConnection::establish(":memory:").expect("open");
        sql_query(&all[0]).execute(&mut conn).expect("DDL");
        sql_query(&all[1])
            .bind::<diesel::sql_types::Double, _>(1.5_f64)
            .execute(&mut conn)
            .expect("INSERT");

        let raw: i64 =
            t::table.filter(t::id.eq(1_i32)).select(t::amount).first(&mut conn).expect("read");
        // Integer arithmetic recovers the bound value without f64 conversion.
        let divisor = 10_i64.pow(scale);
        assert_eq!(raw / divisor, 1_i64, "integer part of 1.50");
        assert_eq!(raw % divisor, 50_i64, "fractional part of 1.50 in hundredths");
    }
}

// ── UUID / text representation
// ────────────────────────────────────────────────

mod uuid_text {
    use diesel::{RunQueryDsl, prelude::*, sql_query, sqlite::SqliteConnection};
    use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, UuidRepresentation};

    diesel::table! {
        u (id) {
            id -> Text,
            name -> Text,
        }
    }

    const UUID: &str = "550e8400-e29b-41d4-a716-446655440000";

    fn stmts(pg: &str) -> Vec<String> {
        Pg2Sqlite::default()
            .sql(pg)
            .expect("parse")
            .translate_to_sql(
                &Pg2SqliteOptions::default().with_uuid_representation(UuidRepresentation::Text),
            )
            .expect("translate")
    }

    #[derive(QueryableByName, Debug)]
    struct NameRow {
        #[diesel(sql_type = diesel::sql_types::Text)]
        name: String,
    }

    /// A bound canonical UUID string matches the stored text-representation row
    /// directly.
    #[test]
    fn param_select_finds_row() {
        let all = stmts(
            "CREATE TABLE u (id UUID PRIMARY KEY, name TEXT NOT NULL);
             INSERT INTO u (id, name) VALUES ('550e8400-e29b-41d4-a716-446655440000', 'alice');
             SELECT name FROM u WHERE id = $1;",
        );
        let mut conn = SqliteConnection::establish(":memory:").expect("open");
        sql_query(&all[0]).execute(&mut conn).expect("DDL");
        sql_query(&all[1]).execute(&mut conn).expect("INSERT");

        let probe = all
            .iter()
            .find(|s| s.to_ascii_uppercase().trim_start().starts_with("SELECT"))
            .expect("SELECT");
        // Text representation stores TEXT; typed DSL would differ from the
        // translator's output.
        let rows: Vec<NameRow> = sql_query(probe)
            .bind::<diesel::sql_types::Text, _>(UUID)
            .load(&mut conn)
            .expect("probe");
        assert_eq!(rows.len(), 1, "canonical UUID bind must find the row");
        assert_eq!(rows[0].name, "alice");
    }

    /// Inserting with a bound canonical UUID string stores the text form
    /// unchanged.
    #[test]
    fn param_insert_stores_canonical_form() {
        let all = stmts(
            "CREATE TABLE u (id UUID PRIMARY KEY, name TEXT NOT NULL);
             INSERT INTO u (id, name) VALUES ($1, 'alice');",
        );
        let mut conn = SqliteConnection::establish(":memory:").expect("open");
        sql_query(&all[0]).execute(&mut conn).expect("DDL");
        // Translator-emitted INSERT tested; text representation needs no
        // conversion.
        let insert = all
            .iter()
            .find(|s| s.to_ascii_uppercase().trim_start().starts_with("INSERT"))
            .expect("INSERT");
        sql_query(insert)
            .bind::<diesel::sql_types::Text, _>(UUID)
            .execute(&mut conn)
            .expect("INSERT must succeed");

        let stored: String = u::table.select(u::id).first(&mut conn).expect("read");
        assert_eq!(stored, UUID);
    }
}

// ── UUID / blob representation
// ────────────────────────────────────────────────

mod uuid_blob {
    use diesel::{RunQueryDsl, prelude::*, sql_query, sqlite::SqliteConnection};
    use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, UuidRepresentation};

    diesel::table! {
        u (id) {
            id -> Binary,
            name -> Text,
        }
    }

    const UUID: &str = "550e8400-e29b-41d4-a716-446655440000";
    const UUID_BYTES: [u8; 16] = [
        0x55, 0x0e, 0x84, 0x00, 0xe2, 0x9b, 0x41, 0xd4, 0xa7, 0x16, 0x44, 0x66, 0x55, 0x44, 0x00,
        0x00,
    ];

    fn stmts(pg: &str) -> Vec<String> {
        Pg2Sqlite::default()
            .sql(pg)
            .expect("parse")
            .translate_to_sql(
                &Pg2SqliteOptions::default().with_uuid_representation(UuidRepresentation::Blob),
            )
            .expect("translate")
    }

    #[derive(QueryableByName, Debug)]
    struct NameRow {
        #[diesel(sql_type = diesel::sql_types::Text)]
        name: String,
    }

    /// A bound canonical UUID string must find the stored 16-byte blob.
    /// Before the fix the emitted WHERE compared the blob against raw text.
    #[test]
    fn param_select_finds_row() {
        let all = stmts(
            "CREATE TABLE u (id UUID PRIMARY KEY, name TEXT NOT NULL);
             INSERT INTO u (id, name) VALUES ('550e8400-e29b-41d4-a716-446655440000', 'alice');
             SELECT name FROM u WHERE id = $1;",
        );
        let mut conn = SqliteConnection::establish(":memory:").expect("open");
        // STRICT and CHECK (length = 16) cannot be expressed in diesel's typed
        // DSL.
        sql_query(&all[0]).execute(&mut conn).expect("DDL");
        sql_query(&all[1]).execute(&mut conn).expect("INSERT");

        let probe = all
            .iter()
            .find(|s| s.to_ascii_uppercase().trim_start().starts_with("SELECT"))
            .expect("SELECT");
        // After the fix translator wraps ?1 in unhex/replace; typed DSL cannot
        // reproduce it.
        let rows: Vec<NameRow> = sql_query(probe)
            .bind::<diesel::sql_types::Text, _>(UUID)
            .load(&mut conn)
            .expect("probe");
        assert_eq!(rows.len(), 1, "canonical UUID text bind must find the blob row");
        assert_eq!(rows[0].name, "alice");
    }

    /// Inserting with a bound canonical UUID string stores the 16-byte binary
    /// form.
    #[test]
    fn param_insert_stores_correct_bytes() {
        let all = stmts(
            "CREATE TABLE u (id UUID PRIMARY KEY, name TEXT NOT NULL);
             INSERT INTO u (id, name) VALUES ($1, 'alice');",
        );
        let mut conn = SqliteConnection::establish(":memory:").expect("open");
        sql_query(&all[0]).execute(&mut conn).expect("DDL");
        // Translator wraps ?1 in unhex/replace so the TEXT bind produces BLOB
        // storage.
        let insert = all
            .iter()
            .find(|s| s.to_ascii_uppercase().trim_start().starts_with("INSERT"))
            .expect("INSERT");
        sql_query(insert)
            .bind::<diesel::sql_types::Text, _>(UUID)
            .execute(&mut conn)
            .expect("INSERT must succeed");

        let stored: Vec<u8> = u::table.select(u::id).first(&mut conn).expect("read");
        assert_eq!(stored, UUID_BYTES);
    }
}

// ── pgvector
// ──────────────────────────────────────────────────────────────────

/// A vector INSERT placeholder must be wrapped with vec_f32() in the emitted
/// SQL. Execution requires the sqlite-vec extension and is not tested here.
#[test]
fn vector_param_insert_is_wrapped_in_translation() {
    let opts = Pg2SqliteOptions::default();
    let stmts = Pg2Sqlite::default()
        .sql(
            "CREATE TABLE t (id INT PRIMARY KEY, embedding vector(3));
             INSERT INTO t (id, embedding) VALUES (1, $1);",
        )
        .expect("parse")
        .translate_to_sql(&opts)
        .expect("translate");
    let insert = stmts
        .iter()
        .find(|s| s.to_ascii_uppercase().trim_start().starts_with("INSERT"))
        .expect("INSERT");
    assert!(
        insert.to_lowercase().contains("vec_f32"),
        "vector INSERT must wrap the placeholder: {insert}"
    );
}

// ── array parameters
// ──────────────────────────────────────────────────────────

/// Array bind parameters are refused at translation time.
/// A caller must bind a JSON array string directly for
/// ArrayRepresentation::Json columns.
#[test]
fn array_param_insert_is_refused() {
    let opts = Pg2SqliteOptions::default().with_array_representation(ArrayRepresentation::Json);
    let result = Pg2Sqlite::default()
        .sql(
            "CREATE TABLE t (id INT PRIMARY KEY, tags TEXT[]);
             INSERT INTO t (id, tags) VALUES (1, $1);",
        )
        .expect("parse")
        .translate(&opts);
    assert!(
        result.is_err(),
        "array placeholder INSERT must be refused; translated to: {:?}",
        result.ok()
    );
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("tags") || msg.contains("array") || msg.contains("JSON"),
        "error must name the column or format issue: {msg}"
    );
}
