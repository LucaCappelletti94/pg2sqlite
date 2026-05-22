//! Red tests: INSERT and UPDATE on `vector` / `halfvec` columns must wrap
//! text-literal values with `vec_f32(...)` / `vec_f16(...)` so the BLOB
//! STRICT main table accepts them. pgvector accepts `'[0.1,0.2,0.3]'` as
//! input because it ships a text-input function on the `vector` type;
//! SQLite has no equivalent coercion, so the translator has to insert the
//! conversion call explicitly.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

fn translate(sql: &str) -> String {
    let options = Pg2SqliteOptions::default();
    let stmts =
        Pg2Sqlite::default().sql(sql).expect("parse").translate(&options).expect("translate");
    stmts.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n")
}

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
}

#[test]
fn insert_default_at_vector_position_left_alone() {
    let sql = "
        CREATE TABLE items (id INTEGER PRIMARY KEY, embedding vector(3));
        INSERT INTO items (id, embedding) VALUES (1, DEFAULT);
    ";
    let out = translate(sql);
    let insert = find_insert(&out);
    assert!(!insert.contains("vec_f32(DEFAULT)"), "DEFAULT must not be wrapped; got: {insert}");
    assert!(insert.to_uppercase().contains("DEFAULT"), "DEFAULT must survive; got: {insert}");
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
}

#[test]
fn insert_from_select_not_modified() {
    // INSERT INTO ... SELECT carries arbitrary row shapes through a query;
    // we cannot tell statically whether the column already produces BLOB.
    // The translator must leave the SELECT-sourced INSERT alone.
    let sql = "
        CREATE TABLE items (id INTEGER PRIMARY KEY, embedding vector(3));
        CREATE TABLE other (id INTEGER PRIMARY KEY, embedding vector(3));
        INSERT INTO items (id, embedding) SELECT id, embedding FROM other;
    ";
    let out = translate(sql);
    let insert = find_insert(&out);
    assert!(!insert.contains("vec_f32"), "INSERT...SELECT must not be modified; got: {insert}");
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
}
