//! Parity for conflict clauses, RETURNING and the DEFAULT keyword.
//!
//! One table is created on both engines from the same PostgreSQL source.
//! Every required DML shape is driven against both engines and the results
//! compared. A disagreement here is a finding about the translator.
//!
//! Most shapes use the same diesel typed DSL on both connections, which
//! keeps the schema in the type system. Three shapes genuinely cannot be
//! expressed in diesel typed form and therefore fall back to engine-specific
//! SQL with a note explaining why:
//!
//! * ON CONFLICT ON CONSTRAINT: diesel has no per-column conflict target for
//!   SQLite, so the PG side uses diesel's `on_constraint` and the SQLite side
//!   runs the translated statement (where the translator resolves the named
//!   constraint to the column list SQLite requires).
//!
//! * VALUES (..., DEFAULT): the DEFAULT keyword inside a VALUES row is not
//!   expressible in diesel's typed insert DSL.
//!
//! * UPDATE SET col = DEFAULT: the DEFAULT keyword in a SET clause is not
//!   expressible in diesel's typed update DSL.

#![allow(clippy::too_many_lines)]

use diesel::{
    connection::SimpleConnection, pg::PgConnection, prelude::*, sqlite::SqliteConnection,
};
use helpers::establish_connection;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use postgres_harness::Outcome;

use crate::{helpers, postgres_harness};

/// PostgreSQL source for the table under test.
///
/// id: SERIAL (database-filled, auto-increment primary key)
/// slug: nullable, UNIQUE (conflict target)
/// score: literal default 7
/// label: function default lower('INITIAL') = 'initial'
const SCHEMA: &str = "
CREATE TABLE items (
    id SERIAL PRIMARY KEY,
    slug TEXT,
    score INTEGER NOT NULL DEFAULT 7,
    label TEXT DEFAULT lower('INITIAL'),
    CONSTRAINT uq_slug UNIQUE (slug)
);";

mod schema {
    diesel::table! {
        items (id) {
            id -> Integer,
            slug -> Nullable<Text>,
            score -> Integer,
            label -> Nullable<Text>,
        }
    }
}

use schema::items;

fn options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
}

fn sqlite_ddl() -> Vec<String> {
    Pg2Sqlite::default()
        .sql(SCHEMA)
        .expect("parse schema")
        .translate_to_sql(&options())
        .expect("translate schema")
}

/// Translates a PostgreSQL DML statement in the context of SCHEMA and returns
/// only the DML portion of the output.
fn sqlite_dml(pg_dml: &str) -> Vec<String> {
    let all = Pg2Sqlite::default()
        .sql(SCHEMA)
        .expect("parse schema")
        .sql(pg_dml)
        .expect("parse dml")
        .translate_to_sql(&options())
        .expect("translate");
    let skip = sqlite_ddl().len();
    all.into_iter().skip(skip).collect()
}

fn pg_setup() -> PgConnection {
    let mut conn = postgres_harness::fresh_database();
    postgres_harness::apply(&mut conn, SCHEMA).expect("DDL accepted by PostgreSQL");
    conn
}

fn sq_setup() -> SqliteConnection {
    let mut conn = establish_connection();
    for stmt in sqlite_ddl() {
        diesel::sql_query(stmt).execute(&mut conn).expect("DDL accepted by SQLite");
    }
    conn
}

/// Runs the SQLite translation of pg_dml and returns the combined outcome.
///
/// Raw sql_query is the only option here because the translated statements
/// exist only at runtime (the translator produces them from the PG source).
fn run_on_sqlite(conn: &mut SqliteConnection, pg_dml: &str) -> Outcome {
    for stmt in sqlite_dml(pg_dml) {
        let r: QueryResult<usize> = diesel::sql_query(stmt.as_str()).execute(conn);
        if r.is_err() {
            return Outcome::Refused;
        }
    }
    Outcome::Accepted
}

struct PgRunner {
    conn: PgConnection,
}

struct SqRunner {
    conn: SqliteConnection,
}

/// Generates the diesel typed DML steps that compile identically for both
/// PgConnection and SqliteConnection.
macro_rules! impl_common {
    ($t:ident) => {
        impl $t {
            fn count(&mut self) -> i64 {
                items::table.count().get_result::<i64>(&mut self.conn).expect("count items")
            }

            fn score(&mut self, slug: &str) -> i32 {
                items::table
                    .filter(items::slug.eq(slug))
                    .select(items::score)
                    .first::<i32>(&mut self.conn)
                    .expect("score for slug")
            }

            fn label(&mut self, slug: &str) -> Option<String> {
                items::table
                    .filter(items::slug.eq(slug))
                    .select(items::label)
                    .first::<Option<String>>(&mut self.conn)
                    .expect("label for slug")
            }

            fn seed(&mut self, slug: &str, score: i32) {
                diesel::insert_into(items::table)
                    .values((items::slug.eq(slug), items::score.eq(score)))
                    .execute(&mut self.conn)
                    .expect("seed row");
            }

            fn insert_do_nothing(&mut self, slug: &str, score: i32) -> Outcome {
                Outcome::of(
                    &diesel::insert_into(items::table)
                        .values((items::slug.eq(slug), items::score.eq(score)))
                        .on_conflict_do_nothing()
                        .execute(&mut self.conn),
                )
            }

            fn insert_do_update_on_col(&mut self, slug: &str, new_score: i32) -> Outcome {
                Outcome::of(
                    &diesel::insert_into(items::table)
                        .values((items::slug.eq(slug), items::score.eq(new_score)))
                        .on_conflict(items::slug)
                        .do_update()
                        .set(items::score.eq(diesel::upsert::excluded(items::score)))
                        .execute(&mut self.conn),
                )
            }

            fn insert_default_values(&mut self) -> Outcome {
                Outcome::of(
                    &diesel::insert_into(items::table).default_values().execute(&mut self.conn),
                )
            }

            fn insert_multi_do_nothing(&mut self) -> Outcome {
                // ('zeta', 6) is new; ('alpha', 99) conflicts on slug.
                Outcome::of(
                    &diesel::insert_into(items::table)
                        .values(vec![
                            (items::slug.eq("zeta"), items::score.eq(6)),
                            (items::slug.eq("alpha"), items::score.eq(99)),
                        ])
                        .on_conflict_do_nothing()
                        .execute(&mut self.conn),
                )
            }

            fn update_score_direct(&mut self, slug: &str, score: i32) {
                diesel::update(items::table.filter(items::slug.eq(slug)))
                    .set(items::score.eq(score))
                    .execute(&mut self.conn)
                    .expect("update score");
            }
        }
    };
}

impl_common!(PgRunner);
impl_common!(SqRunner);

impl PgRunner {
    /// ON CONFLICT ON CONSTRAINT via diesel typed DSL (PG-specific).
    ///
    /// diesel's `on_constraint` is a PG feature; SQLite requires a column
    /// list instead, which the translator supplies.
    fn insert_on_constraint(&mut self, slug: &str, new_score: i32) -> Outcome {
        use diesel::upsert::on_constraint;
        Outcome::of(
            &diesel::insert_into(items::table)
                .values((items::slug.eq(slug), items::score.eq(new_score)))
                .on_conflict(on_constraint("uq_slug"))
                .do_update()
                .set(items::score.eq(diesel::upsert::excluded(items::score)))
                .execute(&mut self.conn),
        )
    }

    /// INSERT ... RETURNING via diesel typed DSL (PG has full RETURNING
    /// support).
    ///
    /// slug is caller-written; score is database-filled from DEFAULT 7.
    fn insert_returning(&mut self, slug: &str) -> Vec<(Option<String>, i32)> {
        diesel::insert_into(items::table)
            .values(items::slug.eq(slug))
            .returning((items::slug, items::score))
            .get_results::<(Option<String>, i32)>(&mut self.conn)
            .expect("RETURNING on PG")
    }

    /// VALUES (..., DEFAULT) for a literal default: DEFAULT in the score
    /// position.
    ///
    /// batch_execute is used because diesel's typed insert DSL has no way to
    /// emit the DEFAULT keyword inside a VALUES row.
    fn insert_values_default_literal(&mut self, slug: &str) -> Outcome {
        Outcome::of(&self.conn.batch_execute(&format!(
            "INSERT INTO items (slug, score, label) VALUES ('{slug}', DEFAULT, 'given')"
        )))
    }

    /// VALUES (..., DEFAULT) for a function default: DEFAULT in the label
    /// position.
    ///
    /// Same reasoning as insert_values_default_literal.
    fn insert_values_default_function(&mut self, slug: &str) -> Outcome {
        Outcome::of(&self.conn.batch_execute(&format!(
            "INSERT INTO items (slug, score, label) VALUES ('{slug}', 5, DEFAULT)"
        )))
    }

    /// UPDATE ... SET col = DEFAULT via batch_execute.
    ///
    /// diesel's typed update DSL cannot emit the DEFAULT keyword in a SET
    /// clause, so raw SQL is the only option on both sides.
    fn update_set_default(&mut self, slug: &str) -> Outcome {
        Outcome::of(
            &self
                .conn
                .batch_execute(&format!("UPDATE items SET score = DEFAULT WHERE slug = '{slug}'")),
        )
    }
}

impl SqRunner {
    /// ON CONFLICT ON CONSTRAINT: the translator resolves the named constraint
    /// to the column list `(slug)` that SQLite requires.
    fn insert_on_constraint(&mut self, slug: &str, new_score: i32) -> Outcome {
        run_on_sqlite(
            &mut self.conn,
            &format!(
                "INSERT INTO items (slug, score) VALUES ('{slug}', {new_score}) \
                 ON CONFLICT ON CONSTRAINT uq_slug DO UPDATE SET score = EXCLUDED.score;"
            ),
        )
    }

    /// INSERT ... RETURNING via translated SQL and sql_query.
    ///
    /// diesel's `.returning()` on SqliteConnection requires the
    /// `returning_clauses_for_sqlite_3_35` feature, which is not enabled in
    /// this crate's dev-dependencies. The translated statement is correct
    /// SQLite 3.35+ syntax, and sql_query captures the returned rows.
    ///
    /// slug is caller-written; score is database-filled from DEFAULT 7.
    fn insert_returning(&mut self, slug: &str) -> Vec<(Option<String>, i32)> {
        let stmts = sqlite_dml(&format!(
            "INSERT INTO items (slug) VALUES ('{slug}') RETURNING slug, score;"
        ));
        assert_eq!(stmts.len(), 1, "RETURNING must translate to one statement");
        #[derive(QueryableByName)]
        struct Row {
            #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
            slug: Option<String>,
            #[diesel(sql_type = diesel::sql_types::Integer)]
            score: i32,
        }
        diesel::sql_query(stmts[0].as_str())
            .load::<Row>(&mut self.conn)
            .expect("RETURNING on SQLite")
            .into_iter()
            .map(|r| (r.slug, r.score))
            .collect()
    }

    /// VALUES (..., DEFAULT) for a literal default: the translator substitutes
    /// 7.
    fn insert_values_default_literal(&mut self, slug: &str) -> Outcome {
        run_on_sqlite(
            &mut self.conn,
            &format!("INSERT INTO items (slug, score, label) VALUES ('{slug}', DEFAULT, 'given');"),
        )
    }

    /// VALUES (..., DEFAULT) for a function default: the translator substitutes
    /// the evaluated lower('INITIAL') expression.
    fn insert_values_default_function(&mut self, slug: &str) -> Outcome {
        run_on_sqlite(
            &mut self.conn,
            &format!("INSERT INTO items (slug, score, label) VALUES ('{slug}', 5, DEFAULT);"),
        )
    }

    /// UPDATE ... SET col = DEFAULT: the translator substitutes the literal 7.
    fn update_set_default(&mut self, slug: &str) -> Outcome {
        run_on_sqlite(
            &mut self.conn,
            &format!("UPDATE items SET score = DEFAULT WHERE slug = '{slug}';"),
        )
    }
}

/// The two engines agree on all conflict, RETURNING and DEFAULT shapes.
#[test]
fn conflict_and_default_shapes_agree() {
    let mut pg = PgRunner { conn: pg_setup() };
    let mut sq = SqRunner { conn: sq_setup() };

    // Seed: the row that subsequent conflict tests bump against.
    pg.seed("alpha", 1);
    sq.seed("alpha", 1);

    // ON CONFLICT DO NOTHING: non-conflicting row is accepted and stored.
    {
        let po = pg.insert_do_nothing("beta", 2);
        let so = sq.insert_do_nothing("beta", 2);
        assert_eq!(po, so, "DO NOTHING (non-conflicting): engines disagree");
        assert_eq!(po, Outcome::Accepted, "DO NOTHING (non-conflicting): must be accepted");
    }

    // ON CONFLICT DO NOTHING: conflicting row is accepted but the stored data
    // is unchanged.
    {
        let po = pg.insert_do_nothing("alpha", 99);
        let so = sq.insert_do_nothing("alpha", 99);
        assert_eq!(po, so, "DO NOTHING (conflicting): engines disagree");
        assert_eq!(
            po,
            Outcome::Accepted,
            "DO NOTHING (conflicting): conflict must be suppressed without error"
        );
        assert_eq!(pg.score("alpha"), sq.score("alpha"), "DO NOTHING: stored score disagrees");
        assert_eq!(pg.score("alpha"), 1, "DO NOTHING: conflicting row must not overwrite alpha");
        assert_eq!(pg.count(), sq.count(), "DO NOTHING: count disagrees");
    }

    // ON CONFLICT (col) DO UPDATE: update lands on the conflicting row.
    {
        let po = pg.insert_do_update_on_col("alpha", 55);
        let so = sq.insert_do_update_on_col("alpha", 55);
        assert_eq!(po, so, "DO UPDATE on column: engines disagree on acceptance");
        assert_eq!(
            pg.score("alpha"),
            sq.score("alpha"),
            "DO UPDATE on column: stored score disagrees"
        );
        assert_eq!(
            pg.score("alpha"),
            55,
            "DO UPDATE on column: update must land with EXCLUDED.score"
        );
    }

    // ON CONFLICT ON CONSTRAINT: named constraint resolved to column list for
    // SQLite.
    {
        let po = pg.insert_on_constraint("alpha", 77);
        let so = sq.insert_on_constraint("alpha", 77);
        assert_eq!(po, so, "ON CONSTRAINT: engines disagree on acceptance");
        assert_eq!(pg.score("alpha"), sq.score("alpha"), "ON CONSTRAINT: stored score disagrees");
        assert_eq!(pg.score("alpha"), 77, "ON CONSTRAINT: update must land");
    }

    // INSERT ... RETURNING: slug is caller-written, score is database-filled
    // (DEFAULT 7).
    {
        let pr = pg.insert_returning("gamma");
        let sr = sq.insert_returning("gamma");
        assert_eq!(pr, sr, "RETURNING: rows disagree between engines");
        assert_eq!(pr.len(), 1, "RETURNING: must return exactly one row");
        assert_eq!(
            pr[0].0.as_deref(),
            Some("gamma"),
            "RETURNING: caller-written slug must come back"
        );
        assert_eq!(pr[0].1, 7, "RETURNING: database-filled score (DEFAULT 7) must come back");
    }

    // INSERT DEFAULT VALUES: all columns receive their declared defaults.
    {
        let pg_before = pg.count();
        let sq_before = sq.count();
        let po = pg.insert_default_values();
        let so = sq.insert_default_values();
        assert_eq!(po, so, "INSERT DEFAULT VALUES: engines disagree");
        assert_eq!(po, Outcome::Accepted, "INSERT DEFAULT VALUES: must be accepted");
        assert_eq!(pg.count() - pg_before, 1, "INSERT DEFAULT VALUES: PG must add exactly one row");
        assert_eq!(
            sq.count() - sq_before,
            1,
            "INSERT DEFAULT VALUES: SQLite must add exactly one row"
        );
    }

    // VALUES (..., DEFAULT) for a literal default: score column uses DEFAULT 7.
    {
        let po = pg.insert_values_default_literal("lit-def");
        let so = sq.insert_values_default_literal("lit-def");
        assert_eq!(po, so, "VALUES DEFAULT literal: engines disagree");
        assert_eq!(
            pg.score("lit-def"),
            sq.score("lit-def"),
            "VALUES DEFAULT literal: stored score disagrees"
        );
        assert_eq!(pg.score("lit-def"), 7, "VALUES DEFAULT literal: score must be 7");
    }

    // VALUES (..., DEFAULT) for a function default: label uses DEFAULT
    // lower('INITIAL').
    {
        let po = pg.insert_values_default_function("fn-def");
        let so = sq.insert_values_default_function("fn-def");
        assert_eq!(po, so, "VALUES DEFAULT function: engines disagree");
        assert_eq!(
            pg.label("fn-def"),
            sq.label("fn-def"),
            "VALUES DEFAULT function: stored label disagrees"
        );
        assert_eq!(
            pg.label("fn-def"),
            Some("initial".to_owned()),
            "VALUES DEFAULT function: label must be 'initial' from lower('INITIAL')"
        );
    }

    // UPDATE ... SET col = DEFAULT: resets score to the declared default.
    // First change alpha's score to prove the reset actually does something.
    {
        pg.update_score_direct("alpha", 42);
        sq.update_score_direct("alpha", 42);
        let po = pg.update_set_default("alpha");
        let so = sq.update_set_default("alpha");
        assert_eq!(po, so, "UPDATE SET DEFAULT: engines disagree");
        assert_eq!(
            pg.score("alpha"),
            sq.score("alpha"),
            "UPDATE SET DEFAULT: stored score disagrees"
        );
        assert_eq!(
            pg.score("alpha"),
            7,
            "UPDATE SET DEFAULT: score must be restored to declared default"
        );
    }

    // Multi-row insert where one row conflicts: new row lands, conflict
    // suppressed.
    {
        let pg_before = pg.count();
        let sq_before = sq.count();
        let po = pg.insert_multi_do_nothing();
        let so = sq.insert_multi_do_nothing();
        assert_eq!(po, so, "multi-row conflict: engines disagree on acceptance");
        assert_eq!(po, Outcome::Accepted, "multi-row conflict: must be accepted");
        assert_eq!(
            pg.count() - pg_before,
            1,
            "multi-row conflict: PG must add exactly one row (zeta)"
        );
        assert_eq!(
            sq.count() - sq_before,
            1,
            "multi-row conflict: SQLite must add exactly one row (zeta)"
        );
        assert_eq!(
            pg.score("alpha"),
            sq.score("alpha"),
            "multi-row conflict: alpha score disagrees"
        );
        assert_eq!(
            pg.score("alpha"),
            7,
            "multi-row conflict: alpha must not be updated by the suppressed row"
        );
    }
}
