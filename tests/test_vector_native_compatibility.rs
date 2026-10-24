//! Native vector lowering refuses unsupported half-precision values.

mod helpers;

use diesel::{Connection, RunQueryDsl, sql_types::Binary};
use pg2sqlite::{
    errors::{Error, RefusalCategory, TranslationDirection},
    prelude::{Pg2Sqlite, Pg2SqliteOptions},
};
use sql_traits::structs::ParserDB;
use sqlparser::{dialect::PostgreSqlDialect, parser::Parser};

#[diesel::declare_sql_function]
extern "SQL" {
    /// Encodes caller text as binary16 bytes.
    fn vec_f16(value: diesel::sql_types::Text) -> Binary;
}

#[derive(Debug, PartialEq, diesel::QueryableByName)]
struct Encoded {
    #[diesel(sql_type = Binary)]
    encoded: Vec<u8>,
}

#[derive(Debug, PartialEq, diesel::QueryableByName)]
struct Distance {
    #[diesel(sql_type = diesel::sql_types::Text)]
    column_name: String,
    #[diesel(sql_type = diesel::sql_types::Double)]
    distance: f64,
}

/// Executes every emitted statement, loading rows of `T` from any query
/// statement.
fn run_emitted<T>(
    statements: &pg2sqlite::script::Script<sqlparser::ast::Statement>,
    connection: &mut diesel::sqlite::SqliteConnection,
) -> Vec<T>
where
    diesel::query_builder::SqlQuery:
        diesel::query_dsl::LoadQuery<'static, diesel::sqlite::SqliteConnection, T>,
{
    let mut rows = Vec::new();
    for statement in statements.iter() {
        // Emitted statements are dynamic SQL.
        let query = diesel::sql_query(statement.to_string());
        if matches!(statement, sqlparser::ast::Statement::Query(_)) {
            rows = query.load::<T>(connection).unwrap();
        } else {
            query.execute(connection).unwrap();
        }
    }
    rows
}

fn half_schema() -> ParserDB {
    ParserDB::from_statements(
        Parser::parse_sql(
            &PostgreSqlDialect {},
            "CREATE TABLE items(id INT PRIMARY KEY, h halfvec(3), v vector(3));
             CREATE TABLE halves(h halfvec(3));",
        )
        .unwrap(),
        "native-vector".to_owned(),
    )
    .unwrap()
}

fn assert_refusal(source: &str, schema: Option<&ParserDB>) {
    let translator = Pg2Sqlite::default().sql(source).unwrap();
    let options = Pg2SqliteOptions::default();
    let result = match schema {
        Some(schema) => translator.translate_with_schema(schema, &options),
        None => translator.translate(&options),
    };
    let error = result.expect_err(source);
    let Error::TranslationRefusal(refusal) = &error else {
        panic!("unexpected refusal for {source} {error:?}");
    };
    assert_eq!(refusal.direction(), TranslationDirection::PostgreSqlToSqlite);
    assert_eq!(refusal.category(), RefusalCategory::UnrepresentableSemantics);
}

#[test]
fn half_precision_declarations_are_typed_refusals() {
    for source in [
        "CREATE TABLE items(id INT PRIMARY KEY, h halfvec(3));",
        "CREATE TEMP TABLE items(id INT PRIMARY KEY, h halfvec(3));",
        "CREATE TABLE items(id INT PRIMARY KEY, h halfvec);",
        "CREATE TABLE items(id INT PRIMARY KEY, h \"halfvec\"(3));",
        "CREATE TABLE items(id INT PRIMARY KEY, v vector(3), h public.halfvec(3));",
        "CREATE TABLE items(id INT PRIMARY KEY); ALTER TABLE items ADD h halfvec(3);",
        "CREATE TABLE items(id INT PRIMARY KEY, h vector(3)); ALTER TABLE items ALTER COLUMN h TYPE halfvec(3);",
    ] {
        assert_refusal(source, None);
    }
}

#[test]
fn half_precision_casts_are_typed_refusals() {
    for source in [
        "SELECT '[0.1,0.2,0.3]'::halfvec(3);",
        "SELECT '[0.1,0.2,0.3]'::public.halfvec(3);",
        "SELECT CAST(NULL AS halfvec(3));",
        "SELECT $1::halfvec(3);",
        "CREATE TABLE items(id INT PRIMARY KEY, v vector(3)); SELECT v::halfvec FROM items;",
    ] {
        assert_refusal(source, None);
    }
}

#[test]
fn supplied_half_precision_columns_refuse_every_write_form() {
    let schema = half_schema();
    for source in [
        "INSERT INTO items(id,h) VALUES(1,'[0.1,0.2,0.3]');",
        "INSERT INTO items(id,h) VALUES(1,$1);",
        "INSERT INTO items VALUES(1,$1,NULL);",
        "INSERT INTO items(id,h) VALUES(1,NULL);",
        "INSERT INTO items(id,h) VALUES(1,DEFAULT);",
        "INSERT INTO items(id,h) SELECT 1,'[0.1,0.2,0.3]';",
        "INSERT INTO items(id,h) SELECT 1,$1 UNION ALL SELECT 2,$2;",
        "UPDATE items SET h='[0.1,0.2,0.3]';",
        "UPDATE items SET h=$1;",
        "UPDATE items SET (id,h)=(2,$1);",
        "UPDATE items SET h=(SELECT '[0.1,0.2,0.3]');",
        "INSERT INTO items(id) VALUES(1) ON CONFLICT(id) DO UPDATE SET h=$1;",
    ] {
        assert_refusal(source, Some(&schema));
    }
}

#[test]
fn half_precision_native_operations_refuse_scoped_and_nested_values() {
    let schema = half_schema();
    for source in [
        "SELECT h <-> v FROM items;",
        "SELECT v <-> h FROM items;",
        "SELECT '[0.1,0.2,0.3]'::halfvec(3) <-> v FROM items;",
        "SELECT (SELECT '[0.1,0.2,0.3]'::halfvec(3)) <-> v FROM items;",
        "UPDATE items SET id=1 WHERE (SELECT * FROM halves LIMIT 1) <-> v < 1;",
        "DELETE FROM items WHERE (SELECT * FROM halves LIMIT 1) <-> v < 1;",
        "SELECT h <=> v FROM items;",
        "SELECT COALESCE(h,h) <-> v FROM items;",
        "SELECT (CASE WHEN id=1 THEN h ELSE h END) <-> v FROM items;",
        "WITH bound AS (SELECT h AS embedding, v FROM items) SELECT embedding <-> v FROM bound;",
        "SELECT (SELECT h FROM items LIMIT 1) <-> v FROM items;",
        "SELECT (SELECT * FROM halves LIMIT 1) <-> v FROM items;",
        "SELECT (SELECT halves.* FROM halves LIMIT 1) <-> v FROM items;",
        "WITH bound AS (SELECT * FROM halves) SELECT (SELECT * FROM bound LIMIT 1) <-> v FROM items;",
        "SELECT (SELECT q.* FROM (SELECT * FROM halves) q LIMIT 1) <-> v FROM items;",
        "SELECT (SELECT * FROM halves UNION ALL SELECT v FROM items LIMIT 1) <-> v FROM items;",
        "SELECT (SELECT (SELECT * FROM halves LIMIT 1)) <-> v FROM items;",
        "WITH RECURSIVE bound(h) AS (SELECT * FROM halves UNION ALL SELECT h FROM bound WHERE FALSE) SELECT (SELECT * FROM bound LIMIT 1) <-> v FROM items;",
        "WITH bound AS (SELECT * FROM halves) SELECT (SELECT * FROM bound LIMIT 1) <-> '[1,2,3]'::vector(3);",
        "WITH bound AS (SELECT * FROM halves) SELECT (SELECT * FROM bound LIMIT 1) <-> i.v FROM (items i CROSS JOIN items j);",
    ] {
        assert_refusal(source, Some(&schema));
    }
}

#[test]
fn an_ambiguous_distance_operand_is_refused_as_unresolved() {
    let error = Pg2Sqlite::default()
        .sql("SELECT v <-> v FROM items a, items b;")
        .unwrap()
        .translate_with_schema(&half_schema(), &Pg2SqliteOptions::default())
        .unwrap_err();
    assert!(matches!(error, Error::UnresolvedColumnReference { .. }), "{error:?}");
}

#[derive(Debug, PartialEq, diesel::QueryableByName)]
struct AuditRow {
    #[diesel(sql_type = diesel::sql_types::Integer)]
    n: i32,
}

#[test]
fn a_trigger_variable_named_like_a_half_precision_column_is_the_variable() {
    use diesel::connection::SimpleConnection;

    let schema = ParserDB::from_statements(
        Parser::parse_sql(
            &PostgreSqlDialect {},
            "CREATE TABLE items(id INT PRIMARY KEY, h halfvec(3));
             CREATE TABLE audit(n INT);
             CREATE FUNCTION record() RETURNS trigger AS $$
             DECLARE h INT := 2;
             BEGIN IF h = 2 THEN INSERT INTO audit VALUES (h); END IF; RETURN NEW; END
             $$ LANGUAGE plpgsql;",
        )
        .unwrap(),
        "half-trigger".to_owned(),
    )
    .unwrap();
    let statements = Pg2Sqlite::default()
        .sql("CREATE TRIGGER audited AFTER INSERT ON items FOR EACH ROW EXECUTE FUNCTION record();")
        .unwrap()
        .translate_with_schema(&schema, &Pg2SqliteOptions::default())
        .unwrap();
    let mut connection = diesel::sqlite::SqliteConnection::establish(":memory:").unwrap();
    connection
        .batch_execute(
            "CREATE TABLE items(id INTEGER PRIMARY KEY, h BLOB); CREATE TABLE audit(n INTEGER);",
        )
        .unwrap();
    for statement in statements.iter() {
        // Emitted statements are dynamic SQL.
        diesel::sql_query(statement.to_string()).execute(&mut connection).unwrap();
    }
    connection.batch_execute("INSERT INTO items(id) VALUES (1);").unwrap();
    let rows = diesel::sql_query("SELECT n FROM audit").load::<AuditRow>(&mut connection).unwrap();
    assert_eq!(rows, [AuditRow { n: 2 }]);
}

#[test]
fn half_precision_manifest_does_not_claim_native_storage() {
    let translator =
        Pg2Sqlite::default().sql("CREATE TABLE items(id INT PRIMARY KEY, h halfvec(3));").unwrap();
    let Error::TranslationRefusal(refusal) = translator
        .translation_manifest(&Pg2SqliteOptions::default())
        .expect_err("unsupported half-precision storage")
    else {
        panic!("unexpected manifest refusal");
    };
    assert_eq!(refusal.direction(), TranslationDirection::PostgreSqlToSqlite);
    assert_eq!(refusal.category(), RefusalCategory::UnrepresentableSemantics);
}

#[test]
fn half_precision_codec_is_not_an_implicit_native_capability() {
    assert_refusal("SELECT vec_f16('[0.1,0.2,0.3]');", None);
}

#[test]
fn an_explicit_caller_codec_preserves_genuine_binary16_bytes() {
    let options = Pg2SqliteOptions::default().with_user_defined_functions(["vec_f16"]);
    let statements = Pg2Sqlite::default()
        .sql("SELECT vec_f16('[0.1,0.2,0.3]') AS encoded;")
        .unwrap()
        .translate(&options)
        .unwrap();
    let mut connection = diesel::sqlite::SqliteConnection::establish(":memory:").unwrap();
    vec_f16_utils::register_impl(&mut connection, |value: String| {
        value
            .trim()
            .trim_start_matches('[')
            .trim_end_matches(']')
            .split(',')
            .flat_map(|value| helpers::f32_to_f16_le(value.trim().parse::<f32>().unwrap()))
            .collect::<Vec<_>>()
    })
    .unwrap();
    let rows = run_emitted::<Encoded>(&statements, &mut connection);
    assert_eq!(rows, vec![Encoded { encoded: vec![0x66, 0x2e, 0x66, 0x32, 0xcd, 0x34] }]);
}

#[test]
fn float32_distance_executes_with_an_opaque_function_relation() {
    helpers::register_sqlite_vec_once();
    let options = Pg2SqliteOptions::default().with_user_defined_functions(["pragma_table_info"]);
    let statements = Pg2Sqlite::default()
        .sql(
            "CREATE TABLE t(id INT PRIMARY KEY, v vector(3));
             CREATE TABLE u(id INT PRIMARY KEY, v2 vector(3));
             INSERT INTO t VALUES(1,'[1,2,3]');
             INSERT INTO u VALUES(1,'[4,6,3]');
             SELECT g.name AS column_name,v <-> v2 AS distance
             FROM t,u,pragma_table_info('t') g ORDER BY g.name;",
        )
        .unwrap()
        .translate(&options)
        .unwrap();
    let mut connection = diesel::sqlite::SqliteConnection::establish(":memory:").unwrap();
    let rows = run_emitted::<Distance>(&statements, &mut connection);
    assert_eq!(
        rows,
        vec![
            Distance { column_name: "id".to_owned(), distance: 5.0 },
            Distance { column_name: "v".to_owned(), distance: 5.0 },
        ]
    );
}

#[test]
fn an_opaque_column_still_refuses_type_dependent_rewrites() {
    let options = Pg2SqliteOptions::default().with_user_defined_functions(["pragma_table_info"]);
    let error = Pg2Sqlite::default()
        .sql("SELECT g.name || 'x' FROM pragma_table_info('t') g;")
        .unwrap()
        .translate(&options)
        .unwrap_err();
    assert!(matches!(error, Error::UnresolvedColumnReference { .. }), "{error:?}");
}

diesel::table! {
    /// Native vector rows.
    scalar_items (id) {
        /// The row identifier.
        id -> Integer,
        /// The float32 vector bytes.
        v -> Binary,
    }
}

diesel::table! {
    /// A single-column vector relation.
    scalar_vectors (v) {
        /// The float32 vector bytes.
        v -> Binary,
    }
}

#[derive(Debug, PartialEq, diesel::QueryableByName)]
struct ScalarDistance {
    #[diesel(sql_type = diesel::sql_types::Integer)]
    id: i32,
    #[diesel(sql_type = diesel::sql_types::Double)]
    distance: f64,
}

#[test]
fn float32_scalar_outputs_preserve_binding_and_cardinality() {
    use diesel::{ExpressionMethods, connection::SimpleConnection};

    helpers::register_sqlite_vec_once();
    let schema = ParserDB::from_statements(
        Parser::parse_sql(
            &PostgreSqlDialect {},
            "CREATE TABLE scalar_items(id INT PRIMARY KEY,v vector(3));
             CREATE TABLE scalar_vectors(v vector(3));
             CREATE TABLE halves(h halfvec(3));",
        )
        .unwrap(),
        "scalar-vectors".to_owned(),
    )
    .unwrap();
    let options = Pg2SqliteOptions::default().with_user_defined_functions(["pragma_table_info"]);
    let mut connection = diesel::sqlite::SqliteConnection::establish(":memory:").unwrap();
    connection
        .batch_execute(
            "CREATE TABLE scalar_items(id INTEGER PRIMARY KEY,v BLOB);
             CREATE TABLE scalar_vectors(v BLOB);
             CREATE TABLE opaque_vector(\"[4,6,3]\" BLOB);",
        )
        .unwrap();
    let encode =
        |values: [f32; 3]| values.into_iter().flat_map(f32::to_le_bytes).collect::<Vec<_>>();
    diesel::insert_into(scalar_items::table)
        .values([
            (scalar_items::id.eq(1), scalar_items::v.eq(encode([1.0, 2.0, 3.0]))),
            (scalar_items::id.eq(2), scalar_items::v.eq(encode([4.0, 6.0, 3.0]))),
        ])
        .execute(&mut connection)
        .unwrap();
    diesel::insert_into(scalar_vectors::table)
        .values(scalar_vectors::v.eq(encode([4.0, 6.0, 3.0])))
        .execute(&mut connection)
        .unwrap();

    let paired = [ScalarDistance { id: 1, distance: 5.0 }, ScalarDistance { id: 2, distance: 0.0 }];
    let scalar = [ScalarDistance { id: 1, distance: 5.0 }];
    for (source, expected) in [
        (
            "SELECT id,(SELECT * FROM scalar_vectors LIMIT 1) <-> v AS distance FROM scalar_items ORDER BY id;",
            paired.as_slice(),
        ),
        (
            "WITH halves AS (SELECT * FROM scalar_vectors) SELECT id,(SELECT * FROM halves LIMIT 1) <-> v AS distance FROM scalar_items ORDER BY id;",
            paired.as_slice(),
        ),
        (
            "SELECT id,(SELECT q.* FROM (SELECT * FROM scalar_vectors) q LIMIT 1) <-> v AS distance FROM scalar_items ORDER BY id;",
            paired.as_slice(),
        ),
        (
            "SELECT id,(SELECT * FROM scalar_vectors UNION ALL SELECT v FROM scalar_items LIMIT 1) <-> v AS distance FROM scalar_items ORDER BY id;",
            paired.as_slice(),
        ),
        (
            "SELECT id,(SELECT (SELECT * FROM scalar_vectors LIMIT 1)) <-> v AS distance FROM scalar_items ORDER BY id;",
            paired.as_slice(),
        ),
        (
            "SELECT id,(SELECT g.name FROM pragma_table_info('opaque_vector') g LIMIT 1) <-> v AS distance FROM scalar_items ORDER BY id;",
            paired.as_slice(),
        ),
        (
            "WITH halves AS (SELECT * FROM scalar_vectors) SELECT 1 AS id,(SELECT * FROM halves LIMIT 1) <-> '[1,2,3]'::vector(3) AS distance;",
            scalar.as_slice(),
        ),
        (
            "WITH halves AS (SELECT * FROM scalar_vectors) SELECT i.id,(SELECT * FROM halves LIMIT 1) <-> i.v AS distance FROM (scalar_items i CROSS JOIN scalar_vectors b) ORDER BY i.id;",
            paired.as_slice(),
        ),
        (
            "SELECT id,v::vector <-> '[4,6,3]'::vector(3) AS distance FROM scalar_items ORDER BY id;",
            paired.as_slice(),
        ),
    ] {
        let statements = Pg2Sqlite::default()
            .sql(source)
            .unwrap()
            .translate_with_schema(&schema, &options)
            .expect(source);
        let mut rows = Vec::new();
        for statement in statements.iter() {
            // Emitted statements are dynamic SQL.
            let query = diesel::sql_query(statement.to_string());
            if matches!(statement, sqlparser::ast::Statement::Query(_)) {
                rows = query.load::<ScalarDistance>(&mut connection).expect(source);
            } else {
                query.execute(&mut connection).expect(source);
            }
        }
        assert_eq!(rows.as_slice(), expected, "{source}");
    }
}

#[test]
fn float32_bound_parameters_store_native_vectors() {
    use diesel::{connection::SimpleConnection, sql_types::Text};

    helpers::register_sqlite_vec_once();
    let schema = ParserDB::from_statements(
        Parser::parse_sql(
            &PostgreSqlDialect {},
            "CREATE TABLE scalar_items(id INT PRIMARY KEY,v vector(3));
             CREATE TABLE scalar_vectors(v vector(3));",
        )
        .unwrap(),
        "bound-vectors".to_owned(),
    )
    .unwrap();
    let options = Pg2SqliteOptions::default();
    let mut connection = diesel::sqlite::SqliteConnection::establish(":memory:").unwrap();
    connection
        .batch_execute(
            "CREATE TABLE scalar_items(id INTEGER PRIMARY KEY,v BLOB);
             CREATE TABLE scalar_vectors(v BLOB);
             INSERT INTO scalar_vectors VALUES(vec_f32('[4,6,3]'));",
        )
        .unwrap();
    let translate = |source: &str, kind: &str| {
        let statements = Pg2Sqlite::default()
            .sql(source)
            .unwrap()
            .translate_with_schema(&schema, &options)
            .expect(source)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        helpers::user_statement_of(&statements, kind).clone()
    };
    let distances = |connection: &mut diesel::sqlite::SqliteConnection| {
        // Emitted statements are dynamic SQL.
        diesel::sql_query(translate(
            "SELECT i.id,i.v <-> s.v AS distance FROM scalar_items i,scalar_vectors s ORDER BY i.id;",
            "SELECT",
        ))
        .load::<ScalarDistance>(connection)
        .unwrap()
    };
    // Emitted statements are dynamic SQL with a bound text vector.
    diesel::sql_query(translate("INSERT INTO scalar_items(id,v) VALUES(3,$1);", "INSERT"))
        .bind::<Text, _>("[1,2,3]")
        .execute(&mut connection)
        .unwrap();
    assert_eq!(distances(&mut connection), [ScalarDistance { id: 3, distance: 5.0 }]);
    diesel::sql_query(translate("UPDATE scalar_items SET v=$1 WHERE id=3;", "UPDATE"))
        .bind::<Text, _>("[4,6,3]")
        .execute(&mut connection)
        .unwrap();
    assert_eq!(distances(&mut connection), [ScalarDistance { id: 3, distance: 0.0 }]);
}
