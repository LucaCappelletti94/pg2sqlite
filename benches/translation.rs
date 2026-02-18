#![allow(missing_docs)]

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions, SessionVariableMapping, UuidRepresentation},
    traits::TranslationOptions,
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

fn bench_schema_translation(c: &mut Criterion) {
    let options = Pg2SqliteOptions::default().with_uuid_representation(UuidRepresentation::Blob);
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

    let fixtures: &[(&str, &str)] =
        &[("rls_basic_1.2KB", RLS_BASIC_SQL), ("rls_grants_21KB", RLS_GRANTS_SQL)];

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
                black_box(parsed.translate(&options).unwrap())
            });
        });
    }
    group.finish();
}

fn bench_insert_statements(c: &mut Criterion) {
    let options = Pg2SqliteOptions::default();
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
                black_box(parsed.translate(&options).unwrap())
            });
        });
    }
    group.finish();
}

fn bench_update_statements(c: &mut Criterion) {
    let options = Pg2SqliteOptions::default();
    let mut group = c.benchmark_group("statement/update");

    let statements: &[(&str, &str)] =
        &[("simple", UPDATE_SIMPLE), ("multi_column", UPDATE_MULTI_COLUMN)];

    for (name, sql) in statements {
        group.bench_with_input(BenchmarkId::from_parameter(name), *sql, |b, sql| {
            b.iter(|| {
                let parsed = Pg2Sqlite::default().sql(black_box(sql)).unwrap();
                black_box(parsed.translate(&options).unwrap())
            });
        });
    }
    group.finish();
}

fn bench_delete_statements(c: &mut Criterion) {
    let options = Pg2SqliteOptions::default();
    let mut group = c.benchmark_group("statement/delete");

    let statements: &[(&str, &str)] = &[("simple", DELETE_SIMPLE), ("subquery", DELETE_SUBQUERY)];

    for (name, sql) in statements {
        group.bench_with_input(BenchmarkId::from_parameter(name), *sql, |b, sql| {
            b.iter(|| {
                let parsed = Pg2Sqlite::default().sql(black_box(sql)).unwrap();
                black_box(parsed.translate(&options).unwrap())
            });
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_schema_translation,
    bench_rls_translation,
    bench_select_statements,
    bench_insert_statements,
    bench_update_statements,
    bench_delete_statements,
);
criterion_main!(benches);
