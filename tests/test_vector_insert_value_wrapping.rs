//! Red tests: INSERT and UPDATE on `vector` / `halfvec` columns must wrap
//! text-literal values with `vec_f32(...)` / `vec_f16(...)` so the BLOB
//! STRICT main table accepts them. pgvector accepts `'[0.1,0.2,0.3]'` as
//! input because it ships a text-input function on the `vector` type;
//! SQLite has no equivalent coercion, so the translator has to insert the
//! conversion call explicitly.

#[path = "helpers/translate.rs"]
mod translate_helpers;
use translate_helpers::translate_default as translate;

fn find_insert(out: &str) -> String {
    out.lines()
        .filter(|l| l.trim_start().to_ascii_uppercase().starts_with("INSERT INTO ITEMS"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn find_update(out: &str) -> String {
    out.lines()
        .filter(|l| l.trim_start().to_ascii_uppercase().starts_with("UPDATE ITEMS"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn insert_into_vector_column_wraps_text_literal_with_vec_f32() {
    let sql = "
        CREATE TABLE items (id INTEGER PRIMARY KEY, embedding vector(3));
        INSERT INTO items (id, embedding) VALUES (1, '[0.1, 0.2, 0.3]');
    ";
    let out = translate(sql);
    let insert = find_insert(&out);
    assert!(
        insert.contains("vec_f32('[0.1, 0.2, 0.3]')"),
        "expected vec_f32 wrap around the text literal; got: {insert}"
    );
    apply_vector_sql(sql);
}

#[test]
fn insert_into_halfvec_column_wraps_text_literal_with_vec_f16() {
    let sql = "
        CREATE TABLE items (id INTEGER PRIMARY KEY, embedding halfvec(3));
        INSERT INTO items (id, embedding) VALUES (1, '[0.1, 0.2, 0.3]');
    ";
    let out = translate(sql);
    let insert = find_insert(&out);
    assert!(
        insert.contains("vec_f16('[0.1, 0.2, 0.3]')"),
        "expected vec_f16 wrap around the text literal; got: {insert}"
    );
    apply_vector_sql(sql);
}

#[test]
fn insert_positional_without_column_list_wraps_vector_position() {
    let sql = "
        CREATE TABLE items (id INTEGER PRIMARY KEY, embedding vector(3));
        INSERT INTO items VALUES (1, '[0.1, 0.2, 0.3]');
    ";
    let out = translate(sql);
    let insert = find_insert(&out);
    assert!(
        insert.contains("vec_f32('[0.1, 0.2, 0.3]')"),
        "positional INSERT must still wrap the vector position; got: {insert}"
    );
    apply_vector_sql(sql);
}

#[test]
fn insert_multi_row_values_wraps_each_vector_literal() {
    let sql = "
        CREATE TABLE items (id INTEGER PRIMARY KEY, embedding vector(3));
        INSERT INTO items (id, embedding) VALUES
            (1, '[0.1, 0.2, 0.3]'),
            (2, '[0.4, 0.5, 0.6]'),
            (3, '[0.7, 0.8, 0.9]');
    ";
    let out = translate(sql);
    let insert = find_insert(&out);
    assert!(insert.contains("vec_f32('[0.1, 0.2, 0.3]')"), "row 1 must wrap; got: {insert}");
    assert!(insert.contains("vec_f32('[0.4, 0.5, 0.6]')"), "row 2 must wrap; got: {insert}");
    assert!(insert.contains("vec_f32('[0.7, 0.8, 0.9]')"), "row 3 must wrap; got: {insert}");
    apply_vector_sql(sql);
}

#[test]
fn insert_null_at_vector_position_left_alone() {
    let sql = "
        CREATE TABLE items (id INTEGER PRIMARY KEY, embedding vector(3));
        INSERT INTO items (id, embedding) VALUES (1, NULL);
    ";
    let out = translate(sql);
    let insert = find_insert(&out);
    assert!(!insert.contains("vec_f32(NULL)"), "NULL must not be wrapped; got: {insert}");
    assert!(insert.contains("NULL"), "NULL must survive verbatim; got: {insert}");
    apply_vector_sql(sql);
}

/// `DEFAULT` is replaced by the column's default before the wrapper runs, and
/// `embedding` declares none and is nullable, so the row carries an unwrapped
/// `NULL`. Inverted from `insert_default_at_vector_position_left_alone`, which
/// pinned `DEFAULT` surviving into the output, where SQLite rejects it with
/// `near "DEFAULT": syntax error`.
#[test]
fn insert_default_at_vector_position_becomes_an_unwrapped_null() {
    let sql = "
        CREATE TABLE items (id INTEGER PRIMARY KEY, embedding vector(3));
        INSERT INTO items (id, embedding) VALUES (1, DEFAULT);
    ";
    let out = translate(sql);
    let insert = find_insert(&out);
    assert!(!insert.contains("vec_f32("), "a substituted NULL must not be wrapped, got: {insert}");
    assert!(
        !insert.to_uppercase().contains("DEFAULT"),
        "DEFAULT cannot reach SQLite, got: {insert}"
    );
    assert!(insert.contains("NULL"), "the column default is NULL, got: {insert}");
    apply_vector_sql(sql);
}

#[test]
fn insert_already_wrapped_vector_literal_not_double_wrapped() {
    let sql = "
        CREATE TABLE items (id INTEGER PRIMARY KEY, embedding vector(3));
        INSERT INTO items (id, embedding) VALUES (1, vec_f32('[0.1, 0.2, 0.3]'));
    ";
    let out = translate(sql);
    let insert = find_insert(&out);
    let count = insert.matches("vec_f32(").count();
    assert_eq!(count, 1, "must not double-wrap an existing vec_f32 call; got: {insert}");
    apply_vector_sql(sql);
}

#[test]
fn insert_explicit_vector_cast_not_double_wrapped() {
    let sql = "
        CREATE TABLE items (id INTEGER PRIMARY KEY, embedding vector(3));
        INSERT INTO items (id, embedding) VALUES (1, '[0.1, 0.2, 0.3]'::vector);
    ";
    let out = translate(sql);
    let insert = find_insert(&out);
    let count = insert.matches("vec_f32(").count();
    assert_eq!(
        count, 1,
        "::vector cast already produces one vec_f32; the INSERT wrap must not add a second; got: {insert}"
    );
    apply_vector_sql(sql);
}

#[test]
fn insert_non_vector_text_column_not_wrapped() {
    let sql = "
        CREATE TABLE items (id INTEGER PRIMARY KEY, message TEXT);
        INSERT INTO items (id, message) VALUES (1, '[not-a-vector]');
    ";
    let out = translate(sql);
    let insert = find_insert(&out);
    assert!(
        !insert.contains("vec_f32"),
        "non-vector TEXT column must not be wrapped; got: {insert}"
    );
    assert!(insert.contains("'[not-a-vector]'"), "literal must survive; got: {insert}");
    apply_vector_sql(sql);
}

#[test]
fn insert_from_select_not_modified() {
    let sql = "
        CREATE TABLE items (id INTEGER PRIMARY KEY, embedding vector(3));
        CREATE TABLE other (id INTEGER PRIMARY KEY, embedding vector(3));
        INSERT INTO items (id, embedding) SELECT id, embedding FROM other;
    ";
    let out = translate(sql);
    let insert = find_insert(&out);
    assert!(!insert.contains("vec_f32"), "INSERT...SELECT must not be modified; got: {insert}");
    apply_vector_sql(sql);
}

#[test]
fn update_set_vector_column_wraps_text_literal() {
    let sql = "
        CREATE TABLE items (id INTEGER PRIMARY KEY, embedding vector(3));
        UPDATE items SET embedding = '[0.1, 0.2, 0.3]' WHERE id = 1;
    ";
    let out = translate(sql);
    let update = find_update(&out);
    assert!(
        update.contains("vec_f32('[0.1, 0.2, 0.3]')"),
        "UPDATE SET on vector column must wrap; got: {update}"
    );
    apply_vector_sql(sql);
}

#[test]
fn update_set_halfvec_column_wraps_with_vec_f16() {
    let sql = "
        CREATE TABLE items (id INTEGER PRIMARY KEY, embedding halfvec(3));
        UPDATE items SET embedding = '[0.1, 0.2, 0.3]' WHERE id = 1;
    ";
    let out = translate(sql);
    let update = find_update(&out);
    assert!(
        update.contains("vec_f16('[0.1, 0.2, 0.3]')"),
        "UPDATE SET on halfvec column must wrap with vec_f16; got: {update}"
    );
    apply_vector_sql(sql);
}

#[test]
fn update_set_non_vector_column_not_wrapped() {
    let sql = "
        CREATE TABLE items (id INTEGER PRIMARY KEY, label TEXT, embedding vector(3));
        UPDATE items SET label = '[not-a-vector]' WHERE id = 1;
    ";
    let out = translate(sql);
    let update = find_update(&out);
    assert!(!update.contains("vec_f32"), "non-vector column must not wrap; got: {update}");
    apply_vector_sql(sql);
}

#[test]
fn update_already_wrapped_vector_literal_not_double_wrapped() {
    let sql = "
        CREATE TABLE items (id INTEGER PRIMARY KEY, embedding vector(3));
        UPDATE items SET embedding = vec_f32('[0.1, 0.2, 0.3]') WHERE id = 1;
    ";
    let out = translate(sql);
    let update = find_update(&out);
    let count = update.matches("vec_f32(").count();
    assert_eq!(count, 1, "must not double-wrap; got: {update}");
    apply_vector_sql(sql);
}

/// The upsert's assignment list writes into the same BLOB column the plain
/// UPDATE does, so the same text literal has to take the same wrap. Emission
/// is asserted rather than execution because `vec_f32` lives in the sqlite-vec
/// extension, which the bundled SQLite does not carry.
#[test]
fn do_update_on_vector_column_wraps_text_literal_with_vec_f32() {
    let sql = "
        CREATE TABLE items (id INTEGER PRIMARY KEY, embedding vector(2));
        INSERT INTO items VALUES (1, '[3,4]') ON CONFLICT (id) DO UPDATE SET embedding = '[5,6]';
    ";
    let out = translate(sql);
    let insert = find_insert(&out);
    assert!(
        insert.contains("vec_f32('[5,6]')"),
        "expected vec_f32 wrap around the DO UPDATE literal; got: {insert}"
    );
    apply_vector_sql(sql);
}

/// The tuple spelling of the same assignment skipped the wrap even on the
/// plain UPDATE path, since the wrap resolved one column name and a tuple has
/// several.
#[test]
fn tuple_update_on_vector_column_wraps_text_literal_with_vec_f32() {
    let sql = "
        CREATE TABLE items (id INTEGER PRIMARY KEY, n INTEGER, embedding vector(2));
        UPDATE items SET (n, embedding) = (7, '[5,6]') WHERE id = 1;
    ";
    let out = translate(sql);
    let update = find_update(&out);
    assert!(
        update.contains("vec_f32('[5,6]')"),
        "expected vec_f32 wrap inside the tuple assignment; got: {update}"
    );
    apply_vector_sql(sql);
}

/// Translates `pg` and executes every emitted statement against an in-memory
/// SQLite connection with the sqlite-vec extension loaded.
/// rusqlite is used directly because registering sqlite3_auto_extension
/// requires the raw FFI layer that diesel does not expose.
fn apply_vector_sql(pg: &str) {
    use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
    use rusqlite::functions::FunctionFlags;
    static INIT_SQLITE_VEC: std::sync::Once = std::sync::Once::new();
    INIT_SQLITE_VEC.call_once(|| {
        // SAFETY: `sqlite3_vec_init` is the sqlite-vec extension entry point
        // whose real C signature is `(db, pzErrMsg, pApi) -> int`. The crate
        // declares it with an opaque signature, so the transmute restores the
        // one `sqlite3_auto_extension` expects, the same pattern
        // `test_vector_semantic.rs` uses. `Once` keeps the registration
        // single-shot.
        unsafe {
            rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute::<
                *const (),
                unsafe extern "C" fn(
                    *mut rusqlite::ffi::sqlite3,
                    *mut *mut std::os::raw::c_char,
                    *const rusqlite::ffi::sqlite3_api_routines,
                ) -> i32,
            >(
                sqlite_vec::sqlite3_vec_init as *const (),
            )));
        }
    });
    let stmts = Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate(&Pg2SqliteOptions::default())
        .expect("translate");
    let conn = rusqlite::Connection::open_in_memory().expect("in-memory SQLite");
    // sqlite-vec 0.1.9 does not provide vec_f16. We register it here.
    // vec0 0.1.9 stores float16[N] columns as float32 blobs internally (its data
    // validation always requires blobs whose length is divisible by 4). So our
    // vec_f16 shim returns the same float32 encoding that vec_f32 would produce.
    // rusqlite is used directly because diesel does not expose
    // create_scalar_function.
    conn.create_scalar_function(
        "vec_f16",
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| {
            use rusqlite::types::ValueRef;
            match ctx.get_raw(0) {
                ValueRef::Null => Ok(rusqlite::types::Value::Null),
                ValueRef::Text(t) => {
                    let text = String::from_utf8_lossy(t);
                    let trimmed = text.trim().trim_start_matches('[').trim_end_matches(']');
                    // Return float32 little-endian bytes: vec0 0.1.9 requires blobs
                    // divisible by 4 even for float16[N] columns.
                    let bytes: Vec<u8> = trimmed
                        .split(',')
                        .filter_map(|s| s.trim().parse::<f32>().ok())
                        .flat_map(|f| f.to_le_bytes())
                        .collect();
                    Ok(rusqlite::types::Value::Blob(bytes))
                }
                _ => {
                    Err(rusqlite::Error::InvalidFunctionParameterType(
                        0,
                        rusqlite::types::Type::Text,
                    ))
                }
            }
        },
    )
    .expect("register vec_f16");
    for s in &stmts {
        conn.execute_batch(&format!("{s};"))
            .unwrap_or_else(|e| panic!("translated statement must execute: {e}\n{s}"));
    }
}
