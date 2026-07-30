//! The structural guarantee: no statement leaves the translator without an
//! observable outcome.
//!
//! Every statement produces emitted SQLite, a hard error, a `LossyDrop`
//! warning, or is consumed by the translation schema and realised elsewhere in
//! the pipeline. Nothing else is permitted. A silently empty translation is how
//! `TRUNCATE`, `COPY`, `MERGE`, and `EXECUTE` all used to vanish out of a
//! migration directory.
//!
//! The fourth outcome is the one that needs guarding, because it looks exactly
//! like the defect: the translator emits nothing and says nothing. It is
//! correct only where the statement's effect really is realised elsewhere, so
//! [`Outcome::ConsumedBySchema`] is a closed list, asserted below, and adding
//! to it takes a deliberate edit here.
//!
//! Two things this file does NOT do. It does not prove the corpus covers every
//! `sqlparser` variant: driving a corpus from the variant lists is R80's job.
//! And it does not need to catch a NEW upstream variant, because the match in
//! `statement.rs` has no wildcard arm, so an unclassified variant fails to
//! compile.

use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions},
    traits::TranslationOptions,
    warnings::TranslationReport,
};

/// What the translator did with a statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// At least one SQLite statement was emitted.
    Emitted,
    /// Nothing was emitted and a warning says why.
    Warned,
    /// Translation failed with an error naming the construct.
    Rejected,
    /// Nothing was emitted and nothing was warned, because the statement is
    /// realised elsewhere in the pipeline. Only the closed list below may sit
    /// here.
    ConsumedBySchema,
}

/// One statement, the declarations it needs, and the outcome it must produce.
struct Case {
    /// Statements that must precede `sql`, translated on their own to get a
    /// baseline the case is measured against.
    setup: &'static str,
    /// The statement under test.
    sql: &'static str,
    /// Required outcome.
    outcome: Outcome,
}

/// Declares the table almost every case refers to.
const TABLE: &str = "CREATE TABLE t (id INTEGER PRIMARY KEY, a TEXT);";

/// Same, plus a schema, a role, a function, and a policy, for the cases that
/// alter or drop one of those.
const SCHEMA: &str = "CREATE SCHEMA s;";
const ROLE: &str = "CREATE ROLE r;";
const FUNCTION: &str = "CREATE FUNCTION f() RETURNS INT AS $$ SELECT 1 $$ LANGUAGE sql;";
const POLICY: &str = "CREATE POLICY p ON t FOR SELECT USING (true);";

/// `GRANT` and `REVOKE` are resolved against the schema, so the role has to
/// exist before they can be classified.
const TABLE_AND_ROLE: &str = "CREATE TABLE t (id INTEGER PRIMARY KEY, a TEXT); CREATE ROLE r;";

/// `REVOKE` is resolved against the grant it removes, so that has to exist too.
const TABLE_ROLE_AND_GRANT: &str =
    "CREATE TABLE t (id INTEGER PRIMARY KEY, a TEXT); CREATE ROLE r; GRANT SELECT ON t TO r;";

const fn emitted(sql: &'static str) -> Case {
    Case { setup: TABLE, sql, outcome: Outcome::Emitted }
}

const fn warned(sql: &'static str) -> Case {
    Case { setup: TABLE, sql, outcome: Outcome::Warned }
}

const fn rejected(sql: &'static str) -> Case {
    Case { setup: TABLE, sql, outcome: Outcome::Rejected }
}

const fn consumed(setup: &'static str, sql: &'static str) -> Case {
    Case { setup, sql, outcome: Outcome::ConsumedBySchema }
}

/// One case per statement kind reachable from `sqlparser`'s PostgreSQL dialect.
///
/// Reachability was measured rather than read off the dialect gates, which
/// proved unreliable: `SHOW TABLES`, `KILL`, `PRINT`, `THROW`, `USE`, `CACHE
/// TABLE`, and a dozen more parse under the PostgreSQL dialect even though they
/// belong to other databases. The fifteen kinds that genuinely do not parse
/// (`ALTER SESSION`, `COPY INTO`, `CREATE FILE FORMAT`, `CREATE MACRO`, `CREATE
/// STAGE`, `DETACH`, `FLUSH`, `INSTALL`, `LIST`, `LOAD DATA`, `LOCK TABLES`,
/// `OPTIMIZE TABLE`, `PUT`, `REMOVE`, `UNLOCK TABLES`) are absent for that
/// reason and are classified in `statement.rs` all the same.
const CASES: &[Case] = &[
    // Translated.
    emitted("SELECT id FROM t"),
    emitted("INSERT INTO t (id) VALUES (1)"),
    emitted("UPDATE t SET a = 'x'"),
    emitted("DELETE FROM t"),
    emitted("TRUNCATE t"),
    emitted("CREATE TABLE u (id INTEGER PRIMARY KEY)"),
    emitted("CREATE VIEW v AS SELECT id FROM t"),
    emitted("CREATE INDEX i ON t (a)"),
    emitted("CREATE VIRTUAL TABLE fts USING fts5(a)"),
    emitted("ALTER TABLE t ADD COLUMN b TEXT"),
    emitted("ALTER TABLE t RENAME TO t2"),
    emitted("DROP TABLE t"),
    emitted("DROP VIEW IF EXISTS v"),
    emitted("DROP INDEX IF EXISTS i"),
    emitted("ANALYZE t"),
    emitted("EXPLAIN SELECT id FROM t"),
    emitted("PRAGMA foreign_keys = 1"),
    emitted("ATTACH DATABASE 'other.db' AS other"),
    emitted("VACUUM"),
    emitted("START TRANSACTION"),
    emitted("COMMIT"),
    emitted("ROLLBACK"),
    emitted("SAVEPOINT sp"),
    emitted("RELEASE SAVEPOINT sp"),
    // Consumed by the translation schema.
    consumed(TABLE, POLICY),
    consumed(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, a TEXT); CREATE POLICY p ON t FOR SELECT USING (true);",
        "ALTER POLICY p ON t RENAME TO q",
    ),
    consumed(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, a TEXT); CREATE POLICY p ON t FOR SELECT USING (true);",
        "DROP POLICY p ON t",
    ),
    consumed(TABLE, SCHEMA),
    consumed(SCHEMA, "ALTER SCHEMA s RENAME TO s2"),
    // Warned and dropped: publish and subscribe.
    warned("LISTEN ch"),
    warned("UNLISTEN ch"),
    warned("NOTIFY ch, 'payload'"),
    // Warned and dropped: access control.
    warned(ROLE),
    warned("CREATE USER u"),
    warned("ALTER ROLE r WITH LOGIN"),
    warned("ALTER USER r WITH PASSWORD 'p'"),
    Case { setup: TABLE_AND_ROLE, sql: "GRANT SELECT ON t TO r", outcome: Outcome::Warned },
    Case {
        setup: TABLE_ROLE_AND_GRANT,
        sql: "REVOKE SELECT ON t FROM r",
        outcome: Outcome::Warned,
    },
    warned("DENY SELECT ON t TO r"),
    warned("DROP ROLE r"),
    warned("DROP USER r"),
    // Warned and dropped: type-like definitions.
    warned("CREATE TYPE mood AS ENUM ('sad', 'ok')"),
    warned("ALTER TYPE mood RENAME TO feeling"),
    warned("CREATE DOMAIN positive AS INTEGER CHECK (VALUE > 0)"),
    warned("DROP DOMAIN positive"),
    warned("DROP TYPE mood"),
    warned("CREATE COLLATION c (provider = 'icu', locale = 'und')"),
    warned("ALTER COLLATION c REFRESH VERSION"),
    warned("DROP COLLATION c"),
    // Warned and dropped: functions, extensions, sequences. A function no
    // trigger calls is genuinely lost, so it warns. The trigger case is in the
    // consumed block above.
    warned(FUNCTION),
    Case { setup: FUNCTION, sql: "DROP FUNCTION f()", outcome: Outcome::Warned },
    warned("ALTER FUNCTION f() OWNER TO r"),
    warned("CREATE EXTENSION IF NOT EXISTS pgcrypto"),
    warned("DROP EXTENSION pgcrypto"),
    warned("CREATE SEQUENCE seq START WITH 1"),
    warned("DROP SEQUENCE seq"),
    // Warned and dropped: foreign data and credentials.
    warned("CREATE SERVER srv FOREIGN DATA WRAPPER postgres_fdw"),
    warned("CREATE CONNECTOR c TYPE 'mysql'"),
    warned("ALTER CONNECTOR c SET URL 'u'"),
    warned("DROP CONNECTOR c"),
    warned("CREATE SECRET sec (TYPE S3)"),
    warned("DROP SECRET sec"),
    // Warned and dropped: introspection.
    warned("COMMENT ON TABLE t IS 'note'"),
    warned("DESCRIBE t"),
    warned("SHOW search_path"),
    warned("SHOW VARIABLES"),
    warned("SHOW STATUS"),
    warned("SHOW TABLES"),
    warned("SHOW VIEWS"),
    warned("SHOW COLUMNS FROM t"),
    warned("SHOW CREATE TABLE t"),
    warned("SHOW SCHEMAS"),
    warned("SHOW DATABASES"),
    warned("SHOW CATALOGS"),
    warned("SHOW CHARSET"),
    warned("SHOW COLLATION"),
    warned("SHOW FUNCTIONS"),
    warned("SHOW OBJECTS"),
    warned("SHOW PROCESSLIST"),
    // Warned and dropped: administration and hints.
    warned("KILL QUERY 1"),
    warned("WAITFOR DELAY '00:00:01'"),
    warned("MSCK REPAIR TABLE t"),
    warned("LOAD httpfs"),
    warned("CREATE WAREHOUSE w"),
    warned("CACHE TABLE t AS SELECT id FROM t"),
    warned("UNCACHE TABLE t"),
    warned("LOCK TABLE t IN ACCESS SHARE MODE"),
    warned("DISCARD PLANS"),
    warned("DISCARD SEQUENCES"),
    warned("SET statement_timeout = 0"),
    warned("SET LOCAL lock_timeout = 0"),
    warned("SET work_mem = '64MB'"),
    warned("SET enable_seqscan = off"),
    warned("SET client_encoding = 'UTF8'"),
    warned("SET standard_conforming_strings = on"),
    warned("SET check_function_bodies = false"),
    warned("SET client_min_messages = warning"),
    warned("SET xmloption = content"),
    warned("DROP SCHEMA s"),
    warned("DROP STAGE st"),
    // Rejected: data movement.
    rejected("COPY t FROM stdin"),
    rejected("INSERT OVERWRITE DIRECTORY '/tmp' SELECT id FROM t"),
    rejected("EXPORT DATA OPTIONS(uri = 'x') AS SELECT id FROM t"),
    rejected("UNLOAD ('select 1') TO 's3://bucket'"),
    // Rejected: writes with no SQLite form.
    rejected("MERGE INTO t USING t AS s ON t.id = s.id WHEN MATCHED THEN UPDATE SET a = s.a"),
    // Rejected: prepared statements.
    rejected("PREPARE ps AS SELECT id FROM t"),
    rejected("EXECUTE ps"),
    rejected("DEALLOCATE ps"),
    // Rejected: procedural control flow.
    rejected("CASE WHEN 1 = 1 THEN SELECT 1; END CASE"),
    rejected("WHILE 1 = 1 BEGIN SELECT 1; END"),
    rejected("RAISE"),
    rejected("RAISERROR('x', 1, 1)"),
    rejected("THROW"),
    rejected("PRINT 'x'"),
    rejected("RETURN 1"),
    rejected("ASSERT 1 = 1"),
    // Rejected: cursors.
    rejected("DECLARE c CURSOR FOR SELECT id FROM t"),
    rejected("OPEN c"),
    rejected("FETCH NEXT FROM c"),
    rejected("CLOSE c"),
    // Rejected: stored procedures.
    rejected("CALL p()"),
    rejected("CREATE PROCEDURE p() AS BEGIN SELECT 1; END"),
    rejected("DROP PROCEDURE p"),
    // Rejected: session state.
    rejected("SET search_path TO s"),
    rejected("SET TIME ZONE 'UTC'"),
    rejected("SET row_security = off"),
    rejected("SET session_replication_role = replica"),
    rejected("SET transform_null_equals = on"),
    rejected("SET array_nulls = off"),
    rejected("SET gin_fuzzy_search_limit = 100"),
    rejected("SET standard_conforming_strings = off"),
    rejected("RESET ALL"),
    rejected("USE other"),
    rejected("DISCARD ALL"),
    rejected("DISCARD TEMP"),
    // Rejected: extensibility objects.
    rejected("CREATE OPERATOR + (leftarg = INTEGER, rightarg = INTEGER, function = f)"),
    rejected("ALTER OPERATOR + (INTEGER, INTEGER) OWNER TO r"),
    rejected("DROP OPERATOR + (INTEGER, INTEGER)"),
    rejected("CREATE OPERATOR CLASS oc FOR TYPE INTEGER USING btree AS OPERATOR 1 ="),
    rejected("ALTER OPERATOR CLASS oc USING btree RENAME TO oc2"),
    rejected("DROP OPERATOR CLASS oc USING btree"),
    rejected("CREATE OPERATOR FAMILY of USING btree"),
    rejected("ALTER OPERATOR FAMILY of USING btree OWNER TO r"),
    rejected("DROP OPERATOR FAMILY of USING btree"),
    rejected("CREATE TEXT SEARCH CONFIGURATION cfg (PARSER = default)"),
    rejected("ALTER TEXT SEARCH CONFIGURATION cfg OWNER TO r"),
    // Rejected: rename spellings PostgreSQL does not have.
    rejected("RENAME TABLE t TO t2"),
    rejected("ALTER TABLE t RENAME AS t2"),
    rejected("ALTER TABLE t RENAME TO public.t2"),
    // Rejected: redefinitions SQLite cannot apply in place.
    rejected("ALTER INDEX i RENAME TO j"),
    rejected("ALTER VIEW v AS SELECT 1"),
    // Rejected: database-level statements.
    rejected("CREATE DATABASE d"),
    rejected("DROP DATABASE d"),
    rejected("DROP SCHEMA s CASCADE"),
    rejected("DROP MATERIALIZED VIEW mv"),
];

fn report(sql: &str) -> Result<TranslationReport, pg2sqlite::errors::Error> {
    Pg2Sqlite::default().sql(sql)?.translate_with_report(&Pg2SqliteOptions::default())
}

fn outcome_of(case: &Case) -> Outcome {
    let baseline = report(case.setup).expect("the case setup must translate on its own");
    let Ok(full) = report(&format!("{} {};", case.setup, case.sql)) else {
        return Outcome::Rejected;
    };

    if full.statements.len() > baseline.statements.len() {
        Outcome::Emitted
    } else if full.warnings.len() > baseline.warnings.len() {
        Outcome::Warned
    } else {
        Outcome::ConsumedBySchema
    }
}

/// Every statement kind produces the outcome its classification promises. A
/// statement that starts emitting nothing and saying nothing shows up here as
/// an unexpected `ConsumedBySchema`.
#[test]
fn every_statement_kind_produces_an_observable_outcome() {
    let mismatches: Vec<String> = CASES
        .iter()
        .filter_map(|case| {
            let actual = outcome_of(case);
            (actual != case.outcome)
                .then(|| format!("{}: expected {:?}, got {actual:?}", case.sql, case.outcome))
        })
        .collect();

    assert!(
        mismatches.is_empty(),
        "{} of {} statement kinds produced the wrong outcome:\n{}",
        mismatches.len(),
        CASES.len(),
        mismatches.join("\n")
    );
}

/// The silent set is closed. A statement may emit nothing and warn nothing only
/// where the pipeline realises it elsewhere, and each member below says where.
/// Extending this list is a decision, not an accident.
///
/// One member cannot be expressed as a corpus row, because it depends on
/// another statement rather than on its own kind: a `CREATE FUNCTION` is silent
/// when a trigger in the same input executes it, and warned otherwise. That
/// pair is asserted by `a_trigger_function_is_silent_while_a_lone_one_warns`.
#[test]
fn only_the_documented_statements_are_silent() {
    let silent: Vec<&str> = CASES
        .iter()
        .filter(|case| case.outcome == Outcome::ConsumedBySchema)
        .map(|case| case.sql)
        .collect();

    assert_eq!(
        silent,
        vec![
            // Realised as the row level security view and trigger set built
            // from the final policy state.
            "CREATE POLICY p ON t FOR SELECT USING (true);",
            "ALTER POLICY p ON t RENAME TO q",
            "DROP POLICY p ON t",
            // Makes a qualified name resolvable. SQLite has no schema to
            // create, so the qualifier is stripped instead.
            "CREATE SCHEMA s;",
            "ALTER SCHEMA s RENAME TO s2",
        ]
    );
}

/// A `CREATE POLICY` really does reach the output, rather than being silent
/// because it was ignored. Guards the entry above: if policies stopped driving
/// row level security, the silent classification would be a plain drop.
#[test]
fn a_consumed_policy_still_produces_row_level_security() {
    // Row level security emits a validation monitor, which needs an audit table
    // name, so this one case cannot use the default options.
    let options = Pg2SqliteOptions::default().with_rls_audit_table_name("rls_violations");
    let translate = |sql: &str| {
        Pg2Sqlite::default()
            .sql(sql)
            .expect("parse")
            .translate_with_report(&options)
            .expect("translate")
    };

    let with_policy = translate(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, owner TEXT);
         ALTER TABLE t ENABLE ROW LEVEL SECURITY;
         CREATE POLICY p ON t FOR SELECT USING (owner = 'me');",
    );
    let without_policy = translate(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, owner TEXT);
         ALTER TABLE t ENABLE ROW LEVEL SECURITY;",
    );

    assert!(
        with_policy.statements.len() > without_policy.statements.len(),
        "the policy must add statements to the output, otherwise it is being dropped"
    );
    assert!(
        with_policy.statements.iter().any(|statement| statement.to_string().contains("'me'")),
        "the policy predicate must appear in the emitted SQL: {:?}",
        with_policy.statements.iter().map(ToString::to_string).collect::<Vec<_>>()
    );
}

/// A function a trigger executes is not dropped, it is inlined into the
/// trigger, so reporting it as a loss would be false. A function nothing
/// executes IS dropped and must say so. The two halves are asserted together
/// because the distinction is the whole point.
#[test]
fn a_trigger_function_is_silent_while_a_lone_one_warns() {
    const BODY: &str = "CREATE TABLE t (id INTEGER PRIMARY KEY, a TEXT);
         CREATE FUNCTION stamp() RETURNS TRIGGER AS $$
         BEGIN
             UPDATE t SET a = 'stamped' WHERE id = NEW.id;
             RETURN NEW;
         END;
         $$ LANGUAGE plpgsql;";

    let with_trigger = report(&format!(
        "{BODY} CREATE TRIGGER trg AFTER INSERT ON t FOR EACH ROW EXECUTE FUNCTION stamp();"
    ))
    .expect("translate");
    let without_trigger = report(BODY).expect("translate");

    assert!(
        with_trigger.warnings.is_empty(),
        "a function a trigger inlines was not dropped, so nothing should be reported: {:?}",
        with_trigger.warnings
    );
    assert_eq!(
        without_trigger.warnings.len(),
        1,
        "a function nothing executes is dropped and must be reported: {:?}",
        without_trigger.warnings
    );

    // Guards the silence against being vacuous: it is only correct because the
    // body really did reach the output.
    assert!(
        with_trigger.statements.iter().any(|statement| statement.to_string().contains("'stamped'")),
        "the function body must appear inside the emitted trigger: {:?}",
        with_trigger.statements.iter().map(ToString::to_string).collect::<Vec<_>>()
    );
}
