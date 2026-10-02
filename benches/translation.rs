#![allow(missing_docs)]

use core::hint::black_box;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use pg2sqlite::prelude::{
    Error, Pg2Sqlite, Pg2SqliteOptions, SessionVariableMapping, UuidRepresentation,
};

// Schema fixtures loaded at compile time (non-RLS)
const DATA_TYPES_EXTENDED_SQL: &str = include_str!("../tests/fixtures/data_types_extended.sql");
const VIEWS_SQL: &str = include_str!("../tests/fixtures/views.sql");
const GROUPS_SQL: &str = include_str!("../tests/fixtures/groups.sql");
const TRIGGER_ISSUE_SQL: &str = include_str!("../tests/fixtures/trigger_issue.sql");

// RLS fixtures (require session variable configuration)
const RLS_BASIC_SQL: &str = include_str!("../tests/fixtures/rls_basic.sql");
const RLS_GRANTS_SQL: &str = include_str!("../tests/fixtures/rls_grants.sql");

// Statement benchmarks - inline SQL
const SELECT_SIMPLE: &str = "SELECT id, name FROM users WHERE active = true;";
const SELECT_JOIN: &str = r#"
SELECT u.id, u.name, o.id AS order_id, o.total
FROM users u
JOIN orders o ON u.id = o.user_id
WHERE o.status = 'completed';
"#;
const SELECT_SUBQUERY: &str = r#"
SELECT id, name
FROM users
WHERE id IN (SELECT user_id FROM orders WHERE total > 100);
"#;
const SELECT_CTE: &str = r#"
WITH active_users AS (
    SELECT id, name FROM users WHERE active = true
)
SELECT * FROM active_users WHERE name LIKE 'A%';
"#;

const INSERT_SIMPLE: &str =
    "INSERT INTO users (name, email) VALUES ('Alice', 'alice@example.com');";
const INSERT_MULTI_ROW: &str = r#"
INSERT INTO users (name, email) VALUES
    ('Alice', 'alice@example.com'),
    ('Bob', 'bob@example.com'),
    ('Charlie', 'charlie@example.com');
"#;
const INSERT_SELECT: &str = r#"
INSERT INTO archived_users (id, name, email)
SELECT id, name, email FROM users WHERE active = false;
"#;
const INSERT_ON_CONFLICT: &str = r#"
INSERT INTO users (id, name, email)
VALUES (1, 'Alice', 'alice@example.com')
ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name, email = EXCLUDED.email;
"#;

const UPDATE_SIMPLE: &str = "UPDATE users SET active = false WHERE id = 1;";
const UPDATE_MULTI_COLUMN: &str = r#"
UPDATE users
SET name = 'Updated Name', email = 'new@example.com', updated_at = NOW()
WHERE id = 1;
"#;

const DELETE_SIMPLE: &str = "DELETE FROM users WHERE id = 1;";
const DELETE_SUBQUERY: &str = r#"
DELETE FROM orders
WHERE user_id IN (SELECT id FROM users WHERE active = false);
"#;

const STATEMENT_SCHEMA_SQL: &str = r#"
CREATE TABLE users (
    id INTEGER PRIMARY KEY,
    name TEXT,
    email TEXT,
    active BOOLEAN,
    updated_at TIMESTAMPTZ
);
CREATE TABLE orders (
    id INTEGER PRIMARY KEY,
    user_id INTEGER,
    total NUMERIC(10, 2),
    status TEXT
);
CREATE TABLE archived_users (
    id INTEGER PRIMARY KEY,
    name TEXT,
    email TEXT
);
"#;

fn statement_schema() -> sql_traits::structs::ParserDB {
    Pg2Sqlite::default().sql(STATEMENT_SCHEMA_SQL).unwrap().build_schema().unwrap()
}

fn bench_schema_translation(c: &mut Criterion) {
    // `groups.sql` defaults a key to `uuidv7()`, which is refused unless the
    // destination's version 7 generator is named, so the fixtures do not
    // translate without this.
    let options = Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_v7_function_name("uuid7");
    let mut group = c.benchmark_group("schema_translation");

    let fixtures: &[(&str, &str)] = &[
        ("data_types_536B", DATA_TYPES_EXTENDED_SQL),
        ("views_839B", VIEWS_SQL),
        ("trigger_issue_2KB", TRIGGER_ISSUE_SQL),
        ("groups_3.5KB", GROUPS_SQL),
    ];

    for (name, sql) in fixtures {
        group.throughput(Throughput::Bytes(sql.len() as u64));
        group.bench_with_input(BenchmarkId::new("full", name), *sql, |b, sql| {
            b.iter(|| {
                let parsed = Pg2Sqlite::default().sql(black_box(sql)).unwrap();
                black_box(parsed.translate(&options).unwrap())
            });
        });
    }
    group.finish();
}

fn bench_rls_translation(c: &mut Criterion) {
    // RLS fixtures require session variable configuration
    let options = Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_v7_function_name("uuid7")
        .with_session_user_role("authenticated")
        .with_session_variable(SessionVariableMapping::current_user("current_app_user"))
        .with_session_variable(SessionVariableMapping::current_setting(
            "app.user_id",
            "current_app_user",
        ))
        .with_session_variable(SessionVariableMapping::current_setting(
            "app.tenant_id",
            "current_tenant",
        ))
        .with_rls_audit_table_name("rls_violations");

    let mut group = c.benchmark_group("rls_translation");

    // rls_grants.sql references tables groups.sql creates, so the two are
    // benched as one reference-closed document.
    let groups_plus_grants = format!("{GROUPS_SQL}\n{RLS_GRANTS_SQL}");
    let fixtures: &[(&str, &str)] =
        &[("rls_basic_1.2KB", RLS_BASIC_SQL), ("groups_plus_rls_grants_25KB", &groups_plus_grants)];

    for (name, sql) in fixtures {
        group.throughput(Throughput::Bytes(sql.len() as u64));
        group.bench_with_input(BenchmarkId::new("full", name), *sql, |b, sql| {
            let options = options.clone();
            b.iter(|| {
                let parsed = Pg2Sqlite::default().sql(black_box(sql)).unwrap();
                black_box(parsed.translate(&options).unwrap())
            });
        });
    }
    group.finish();
}

fn bench_select_statements(c: &mut Criterion) {
    let options = Pg2SqliteOptions::default();
    let schema = statement_schema();
    let mut group = c.benchmark_group("statement/select");

    let statements: &[(&str, &str)] = &[
        ("simple", SELECT_SIMPLE),
        ("join", SELECT_JOIN),
        ("subquery", SELECT_SUBQUERY),
        ("cte", SELECT_CTE),
    ];

    for (name, sql) in statements {
        group.bench_with_input(BenchmarkId::from_parameter(name), *sql, |b, sql| {
            b.iter(|| {
                let parsed = Pg2Sqlite::default().sql(black_box(sql)).unwrap();
                black_box(parsed.translate_with_schema(&schema, &options).unwrap())
            });
        });
    }
    group.finish();
}

fn bench_insert_statements(c: &mut Criterion) {
    let options = Pg2SqliteOptions::default();
    let schema = statement_schema();
    let mut group = c.benchmark_group("statement/insert");

    let statements: &[(&str, &str)] = &[
        ("simple", INSERT_SIMPLE),
        ("multi_row", INSERT_MULTI_ROW),
        ("select", INSERT_SELECT),
        ("on_conflict", INSERT_ON_CONFLICT),
    ];

    for (name, sql) in statements {
        group.bench_with_input(BenchmarkId::from_parameter(name), *sql, |b, sql| {
            b.iter(|| {
                let parsed = Pg2Sqlite::default().sql(black_box(sql)).unwrap();
                black_box(parsed.translate_with_schema(&schema, &options).unwrap())
            });
        });
    }
    group.finish();
}

fn bench_update_statements(c: &mut Criterion) {
    let options = Pg2SqliteOptions::default();
    let schema = statement_schema();
    let mut group = c.benchmark_group("statement/update");

    let statements: &[(&str, &str)] =
        &[("simple", UPDATE_SIMPLE), ("multi_column", UPDATE_MULTI_COLUMN)];

    for (name, sql) in statements {
        group.bench_with_input(BenchmarkId::from_parameter(name), *sql, |b, sql| {
            b.iter(|| {
                let parsed = Pg2Sqlite::default().sql(black_box(sql)).unwrap();
                black_box(parsed.translate_with_schema(&schema, &options).unwrap())
            });
        });
    }
    group.finish();
}

fn bench_delete_statements(c: &mut Criterion) {
    let options = Pg2SqliteOptions::default();
    let schema = statement_schema();
    let mut group = c.benchmark_group("statement/delete");

    let statements: &[(&str, &str)] = &[("simple", DELETE_SIMPLE), ("subquery", DELETE_SUBQUERY)];

    for (name, sql) in statements {
        group.bench_with_input(BenchmarkId::from_parameter(name), *sql, |b, sql| {
            b.iter(|| {
                let parsed = Pg2Sqlite::default().sql(black_box(sql)).unwrap();
                black_box(parsed.translate_with_schema(&schema, &options).unwrap())
            });
        });
    }
    group.finish();
}

fn bench_translation_samples(c: &mut Criterion) {
    let options = Pg2SqliteOptions::default();
    let schema = statement_schema();
    let mut group = c.benchmark_group("translation_samples");

    let statements: &[(&str, &str)] = &[
        ("select_simple", SELECT_SIMPLE),
        ("select_join", SELECT_JOIN),
        ("select_subquery", SELECT_SUBQUERY),
        ("select_cte", SELECT_CTE),
        ("insert_simple", INSERT_SIMPLE),
        ("insert_on_conflict", INSERT_ON_CONFLICT),
        ("update_multi_column", UPDATE_MULTI_COLUMN),
        ("delete_subquery", DELETE_SUBQUERY),
        (
            "create_table_ddl",
            "CREATE TABLE t (id SERIAL PRIMARY KEY, name TEXT NOT NULL, active BOOLEAN, created_at TIMESTAMPTZ);",
        ),
    ];

    for (name, sql) in statements {
        group.bench_with_input(BenchmarkId::new("pg2sqlite", name), *sql, |b, sql| {
            b.iter(|| {
                let parsed = Pg2Sqlite::default().sql(black_box(sql)).unwrap();
                black_box(parsed.translate_with_schema(&schema, &options).unwrap())
            });
        });
    }
    group.finish();
}

const QUERY_SCHEMA: &str = "CREATE TABLE nums (n INTEGER NOT NULL);";
const QUERY_CASES: &[(&str, &str)] = &[
    ("scalar", "SELECT 0 AS n"),
    ("table", "SELECT n FROM nums WHERE n < 4 ORDER BY n"),
    ("pagination", "SELECT n FROM nums ORDER BY n LIMIT 2 OFFSET 1"),
    (
        "compound",
        "(SELECT n FROM nums ORDER BY n LIMIT 2) UNION ALL (SELECT n FROM nums ORDER BY n DESC LIMIT 2) ORDER BY n",
    ),
    ("cte", "WITH selected AS (SELECT n FROM nums WHERE n < 4) SELECT n FROM selected ORDER BY n"),
];

const CONSUMER_CASES: &[(&str, &str)] = &[
    ("insert_source", "INSERT INTO nums (n) SELECT n FROM nums WHERE n < 4"),
    ("update_scalar", "UPDATE nums SET n = (SELECT MAX(n) FROM nums) WHERE n = 1"),
    ("delete_scalar", "DELETE FROM nums WHERE n = (SELECT MIN(n) FROM nums)"),
    ("view", "CREATE VIEW small_nums AS SELECT n FROM nums WHERE n < 4"),
    ("ctas", "CREATE TABLE small_nums AS SELECT n FROM nums WHERE n < 4"),
];

// Defect lanes require executable SQLite output during setup.
const DEFECT_CASES: &[(&str, &str)] = &[
    (
        "nested_fetch",
        "SELECT n FROM (SELECT n FROM nums ORDER BY n FETCH FIRST 2 ROWS ONLY) t ORDER BY n",
    ),
    (
        "compound_fetch",
        "(SELECT n FROM nums ORDER BY n FETCH FIRST 2 ROWS ONLY) UNION ALL (SELECT n FROM nums ORDER BY n DESC FETCH FIRST 2 ROWS ONLY) ORDER BY n",
    ),
];

fn nested_depth_sql(depth: u32) -> String {
    let mut sql = "SELECT n FROM nums WHERE n < 4".to_string();
    for level in 0..depth {
        sql = format!("SELECT n FROM ({sql}) t{level}");
    }
    format!("{sql} ORDER BY n")
}

fn compound_operands_sql(operands: u32) -> String {
    let branches: Vec<String> = (0..operands)
        .map(|i| {
            format!("(SELECT n + {} AS n FROM nums WHERE n < 4 ORDER BY n LIMIT 2)", i * 10_000)
        })
        .collect();
    format!("{} ORDER BY n", branches.join(" UNION ALL "))
}

fn scaling_query_cases() -> Vec<(String, String)> {
    let mut cases: Vec<(String, String)> =
        [1u32, 4, 16, 32].map(|depth| (format!("depth_{depth}"), nested_depth_sql(depth))).to_vec();
    for operands in [2u32, 8, 32] {
        cases.push((format!("operands_{operands}"), compound_operands_sql(operands)));
    }
    cases
}

fn expected_query_rows(name: &str, size: i32) -> Vec<i32> {
    match name {
        "scalar" => vec![0],
        "table" | "cte" => vec![1, 2, 3],
        "pagination" => vec![2, 3],
        "compound" | "compound_fetch" => vec![1, 2, size - 1, size],
        "nested_fetch" => vec![1, 2],
        name if name.starts_with("depth_") => vec![1, 2, 3],
        name if name.starts_with("operands_") => {
            let operands = name["operands_".len()..].parse::<i32>().unwrap();
            (0..operands).flat_map(|i| [i * 10_000 + 1, i * 10_000 + 2]).collect()
        }
        other => panic!("no expected rows registered for runtime case {other}"),
    }
}

struct QueryShape {
    name: String,
    sql: String,
    parsed: Pg2Sqlite,
    translated: Vec<sqlparser::ast::Statement>,
    query: String,
}

fn rendered_query(statements: &[sqlparser::ast::Statement]) -> String {
    statements
        .iter()
        .filter(|statement| matches!(statement, sqlparser::ast::Statement::Query(_)))
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(";")
}

// A scaling depth can exceed the parser recursion limit, which setup records as
// a refusal lane.
fn prepare_query_shape(
    name: &str,
    sql: &str,
    schema: &sql_traits::structs::ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<QueryShape, Error> {
    let parsed = Pg2Sqlite::default().sql(sql)?;
    let translated = parsed.translate_with_schema(schema, options).unwrap();
    let query = rendered_query(&translated);
    Ok(QueryShape { name: name.to_string(), sql: sql.to_string(), parsed, translated, query })
}

fn fresh_fixture_connection() -> diesel::SqliteConnection {
    use diesel::{connection::SimpleConnection, prelude::*};

    let mut connection = SqliteConnection::establish(":memory:").unwrap();
    connection.batch_execute(QUERY_SCHEMA).unwrap();
    let rows = (1..=5).map(|n| nums::n.eq(n)).collect::<Vec<_>>();
    diesel::insert_into(nums::table).values(&rows).execute(&mut connection).unwrap();
    connection
}

fn shape_rows(shape: &QueryShape) -> Result<Vec<i32>, diesel::result::Error> {
    use diesel::prelude::*;

    // Translated SQL is runtime syntax under test.
    let mut connection = fresh_fixture_connection();
    diesel::sql_query(&shape.query)
        .load::<QueryNumber>(&mut connection)
        .map(|rows| rows.into_iter().map(|row| row.n).collect())
}

fn bench_query_pipeline(
    c: &mut Criterion,
    translator: &Pg2Sqlite,
    schema: &sql_traits::structs::ParserDB,
    options: &Pg2SqliteOptions,
    shape: &QueryShape,
) {
    let name = shape.name.as_str();
    let sql = shape.sql.as_str();
    let parsed = &shape.parsed;
    let translated = &shape.translated;
    let query = shape.query.as_str();
    translator.reverse_sql(query, schema, options).unwrap();
    c.bench_function(&format!("query_shapes/parse/{name}"), |b| {
        b.iter(|| black_box(Pg2Sqlite::default().sql(black_box(sql))));
    });
    c.bench_function(&format!("query_shapes/translate/{name}"), |b| {
        b.iter(|| black_box(parsed.translate_with_schema(schema, options)));
    });
    c.bench_function(&format!("query_shapes/render/{name}"), |b| {
        b.iter(|| black_box(translated.iter().map(ToString::to_string).collect::<Vec<_>>()));
    });
    c.bench_function(&format!("query_shapes/full/{name}"), |b| {
        b.iter(|| {
            black_box(Pg2Sqlite::default().sql(black_box(sql)).and_then(|parsed| {
                parsed.translate_with_schema(schema, options).map(|statements| {
                    statements.iter().map(ToString::to_string).collect::<Vec<_>>()
                })
            }))
        });
    });
    c.bench_function(&format!("query_shapes/reverse/{name}"), |b| {
        b.iter(|| black_box(translator.reverse_sql(black_box(query), schema, options)));
    });
}

fn bench_runtime_shape(c: &mut Criterion, name: &str, query: &str) {
    use diesel::{connection::SimpleConnection, prelude::*};

    for size in [5, 1_000, 10_000] {
        let mut connection = SqliteConnection::establish(":memory:").unwrap();
        connection.batch_execute(QUERY_SCHEMA).unwrap();
        let rows = (1..=size).map(|n| nums::n.eq(n)).collect::<Vec<_>>();
        diesel::insert_into(nums::table).values(&rows).execute(&mut connection).unwrap();
        // Translated SQL is runtime syntax under test.
        let actual = diesel::sql_query(query)
            .load::<QueryNumber>(&mut connection)
            .unwrap()
            .into_iter()
            .map(|row| row.n)
            .collect::<Vec<_>>();
        assert_eq!(actual, expected_query_rows(name, size));
        c.bench_function(&format!("query_runtime/{name}/{size}"), |b| {
            b.iter(|| black_box(diesel::sql_query(query).load::<QueryNumber>(&mut connection)));
        });
    }
}

fn bench_query_shapes(c: &mut Criterion) {
    use diesel::{connection::SimpleConnection, prelude::*};

    let options = Pg2SqliteOptions::default();
    let translator = Pg2Sqlite::default().sql(QUERY_SCHEMA).unwrap();
    let schema = translator.build_schema().unwrap();

    for &(name, sql) in QUERY_CASES {
        let shape = prepare_query_shape(name, sql, &schema, &options).unwrap();
        bench_query_pipeline(c, &translator, &schema, &options, &shape);
    }

    for (name, sql) in scaling_query_cases() {
        match prepare_query_shape(&name, &sql, &schema, &options) {
            Ok(shape) => {
                bench_query_pipeline(c, &translator, &schema, &options, &shape);
            }
            Err(err) => {
                eprintln!(
                    "query_shapes/{name}: lanes skipped, parser refuses the source at setup ({err})"
                );
                c.bench_function(&format!("query_shapes/parse_depth_refusal/{name}"), |b| {
                    b.iter(|| black_box(Pg2Sqlite::default().sql(black_box(&sql))));
                });
            }
        }
    }

    for &(name, sql) in CONSUMER_CASES {
        let parsed = Pg2Sqlite::default().sql(sql).unwrap();
        let translated = parsed.translate_with_schema(&schema, &options).unwrap();
        let mut connection = fresh_fixture_connection();
        for statement in translated.iter().map(ToString::to_string) {
            connection.batch_execute(&statement).unwrap();
        }
        let expected: &[i32] = match name {
            "insert_source" => &[1, 1, 2, 2, 3, 3, 4, 5],
            "update_scalar" => &[2, 3, 4, 5, 5],
            "delete_scalar" => &[2, 3, 4, 5],
            "view" | "ctas" => &[1, 2, 3],
            other => panic!("no expected rows registered for consumer case {other}"),
        };
        let actual = match name {
            "view" | "ctas" => {
                small_nums::table
                    .select(small_nums::n)
                    .order(small_nums::n.asc())
                    .load::<i32>(&mut connection)
                    .unwrap()
            }
            _ => {
                nums::table
                    .select(nums::n)
                    .order(nums::n.asc())
                    .load::<i32>(&mut connection)
                    .unwrap()
            }
        };
        assert_eq!(actual, expected, "{name}");
        c.bench_function(&format!("query_shapes/full/{name}"), |b| {
            b.iter(|| {
                black_box(Pg2Sqlite::default().sql(black_box(sql)).and_then(|parsed| {
                    parsed.translate_with_schema(&schema, &options).map(|statements| {
                        statements.iter().map(ToString::to_string).collect::<Vec<_>>()
                    })
                }))
            });
        });
    }

    for &(name, sql) in DEFECT_CASES {
        let shape = prepare_query_shape(name, sql, &schema, &options).unwrap();
        match shape_rows(&shape) {
            Ok(rows) => {
                assert_eq!(rows, expected_query_rows(name, 5));
                bench_query_pipeline(c, &translator, &schema, &options, &shape);
            }
            Err(err) => {
                eprintln!(
                    "query_shapes/{name}: lanes skipped, SQLite rejects the translated output ({err})"
                );
            }
        }
    }

    let refused = Pg2Sqlite::default()
        .sql("SELECT n FROM nums ORDER BY n FETCH FIRST 2 ROWS WITH TIES")
        .unwrap();
    assert!(refused.translate_with_schema(&schema, &options).is_err());
    c.bench_function("query_shapes/refusal/with_ties", |b| {
        b.iter(|| black_box(refused.translate_with_schema(&schema, &options)));
    });

    // The setup assertion keeps an over-accepting parser from being recorded
    // as a refusal
    let malformed = "SELECT n FROM nums WHERE n =";
    assert!(Pg2Sqlite::default().sql(malformed).is_err());
    c.bench_function("query_shapes/refusal/malformed_source", |b| {
        b.iter(|| black_box(Pg2Sqlite::default().sql(black_box(malformed))));
    });

    for depth in [1, 4, 16, 32] {
        let sql = format!("{}SELECT 0{}", "(".repeat(depth), ")".repeat(depth));
        let accepted = Pg2Sqlite::default().sql(&sql).is_ok();
        let lane = if accepted { "parse_depth" } else { "parse_depth_refusal" };
        c.bench_function(&format!("query_shapes/{lane}/{depth}"), |b| {
            b.iter(|| black_box(Pg2Sqlite::default().sql(black_box(&sql))));
        });
    }
}

diesel::table! {
    nums (n) {
        n -> Integer,
    }
}

diesel::table! {
    small_nums (n) {
        n -> Integer,
    }
}

#[derive(diesel::QueryableByName)]
struct QueryNumber {
    #[diesel(sql_type = diesel::sql_types::Integer)]
    n: i32,
}

fn bench_query_runtime(c: &mut Criterion) {
    let options = Pg2SqliteOptions::default();
    let schema = Pg2Sqlite::default().sql(QUERY_SCHEMA).unwrap().build_schema().unwrap();

    for &(name, sql) in QUERY_CASES {
        let shape = prepare_query_shape(name, sql, &schema, &options).unwrap();
        bench_runtime_shape(c, name, &shape.query);
    }

    for (name, sql) in scaling_query_cases() {
        let Ok(shape) = prepare_query_shape(&name, &sql, &schema, &options) else {
            eprintln!("query_runtime/{name}: lanes skipped, parser refuses the source at setup");
            continue;
        };
        bench_runtime_shape(c, &name, &shape.query);
    }

    for &(name, sql) in DEFECT_CASES {
        let shape = prepare_query_shape(name, sql, &schema, &options).unwrap();
        let Ok(rows) = shape_rows(&shape) else {
            eprintln!("query_runtime/{name}: lanes skipped, SQLite rejects the translated output");
            continue;
        };
        assert_eq!(rows, expected_query_rows(name, 5));
        bench_runtime_shape(c, name, &shape.query);
    }
}

criterion_group!(
    benches,
    bench_schema_translation,
    bench_rls_translation,
    bench_select_statements,
    bench_insert_statements,
    bench_update_statements,
    bench_delete_statements,
    bench_translation_samples,
    bench_query_shapes,
    bench_query_runtime,
);
criterion_main!(benches);
