//! Parity for constraints, defaults and computed columns.
//!
//! A table carrying CHECK, NOT NULL, UNIQUE, two foreign keys (CASCADE and
//! RESTRICT), a literal default, a function default, a generated column, and
//! a VARCHAR(n) is created in both engines from the same PostgreSQL source.
//! Each constraint is driven with an admitted write and a forbidden write; the
//! storage is compared after each. Both FK parent-delete paths are exercised.

#![allow(clippy::too_many_lines)]

use diesel::{pg::PgConnection, prelude::*, sqlite::SqliteConnection};
use helpers::establish_connection;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use postgres_harness::Outcome;

use crate::{helpers, postgres_harness};

/// Three tables: two FK parents and one child carrying all the constraints.
const SOURCE: &str = "
CREATE TABLE parent_cascade (
    id INTEGER PRIMARY KEY,
    label TEXT NOT NULL
);
CREATE TABLE parent_restrict (
    id INTEGER PRIMARY KEY,
    label TEXT NOT NULL
);
CREATE TABLE items (
    id INTEGER PRIMARY KEY,
    score INTEGER NOT NULL CHECK (score >= 0 AND score <= 100),
    name TEXT NOT NULL,
    code TEXT UNIQUE,
    cascade_id INTEGER REFERENCES parent_cascade(id) ON DELETE CASCADE,
    restrict_id INTEGER REFERENCES parent_restrict(id) ON DELETE RESTRICT,
    status TEXT DEFAULT 'pending',
    created_at TEXT DEFAULT now(),
    short_name VARCHAR(10),
    doubled_score INTEGER GENERATED ALWAYS AS (score * 2) STORED
);
";

fn options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
}

mod schema {
    diesel::table! {
        parent_cascade (id) {
            id -> Integer,
            label -> Text,
        }
    }

    diesel::table! {
        parent_restrict (id) {
            id -> Integer,
            label -> Text,
        }
    }

    diesel::table! {
        // name is Nullable here so the diesel DSL can express a NULL value;
        // the database enforces NOT NULL at runtime.
        // doubled_score is included so an explicit insert can be attempted;
        // both engines refuse it because the column is generated.
        items (id) {
            id -> Integer,
            score -> Integer,
            name -> Nullable<Text>,
            code -> Nullable<Text>,
            cascade_id -> Nullable<Integer>,
            restrict_id -> Nullable<Integer>,
            status -> Nullable<Text>,
            created_at -> Nullable<Text>,
            short_name -> Nullable<Text>,
            doubled_score -> Integer,
        }
    }
}

use schema::{items, parent_cascade, parent_restrict};

/// One move in the scenario, named for what it does, not what it expects.
#[derive(Debug)]
enum Step {
    InsertParentCascade {
        id: i32,
        label: &'static str,
    },
    InsertParentRestrict {
        id: i32,
        label: &'static str,
    },

    /// Insert providing id, score, and name; other columns take their defaults.
    InsertItem {
        id: i32,
        score: i32,
        name: &'static str,
    },
    /// Insert omitting name; the NOT NULL column has no default.
    InsertItemWithoutName {
        id: i32,
        score: i32,
    },
    InsertItemWithCode {
        id: i32,
        score: i32,
        name: &'static str,
        code: &'static str,
    },
    InsertItemWithShortName {
        id: i32,
        score: i32,
        name: &'static str,
        short_name: &'static str,
    },
    /// Attempt to supply a value for the generated column.
    InsertItemWithDoubled {
        id: i32,
        score: i32,
        name: &'static str,
        doubled_score: i32,
    },
    InsertItemWithCascadeId {
        id: i32,
        score: i32,
        name: &'static str,
        cascade_id: i32,
    },
    InsertItemWithRestrictId {
        id: i32,
        score: i32,
        name: &'static str,
        restrict_id: i32,
    },

    DeleteParentCascade {
        id: i32,
    },
    DeleteParentRestrict {
        id: i32,
    },

    CountItems,
    CountItemsWhereStatusPending {
        id: i32,
    },
    CountItemsWhereCreatedAtSet {
        id: i32,
    },
    CountItemsWhereDoubledScore {
        id: i32,
        doubled: i32,
    },
    CountItemsWhereCascadeId {
        cascade_id: i32,
    },
    CountParentRestrict,
}

/// The step implementation is identical for both connection types; diesel's
/// typed DSL handles the backend translation.
macro_rules! runner {
    ($name:ident, $conn:ty) => {
        struct $name {
            connection: $conn,
        }

        impl $name {
            fn step(&mut self, step: &Step) -> Outcome {
                match step {
                    Step::InsertParentCascade { id, label } => {
                        Outcome::of(
                            &diesel::insert_into(parent_cascade::table)
                                .values((
                                    parent_cascade::id.eq(*id),
                                    parent_cascade::label.eq(*label),
                                ))
                                .execute(&mut self.connection),
                        )
                    }
                    Step::InsertParentRestrict { id, label } => {
                        Outcome::of(
                            &diesel::insert_into(parent_restrict::table)
                                .values((
                                    parent_restrict::id.eq(*id),
                                    parent_restrict::label.eq(*label),
                                ))
                                .execute(&mut self.connection),
                        )
                    }
                    Step::InsertItem { id, score, name } => {
                        Outcome::of(
                            &diesel::insert_into(items::table)
                                .values((
                                    items::id.eq(*id),
                                    items::score.eq(*score),
                                    items::name.eq(Some(*name)),
                                ))
                                .execute(&mut self.connection),
                        )
                    }
                    Step::InsertItemWithoutName { id, score } => {
                        Outcome::of(
                            &diesel::insert_into(items::table)
                                .values((items::id.eq(*id), items::score.eq(*score)))
                                .execute(&mut self.connection),
                        )
                    }
                    Step::InsertItemWithCode { id, score, name, code } => {
                        Outcome::of(
                            &diesel::insert_into(items::table)
                                .values((
                                    items::id.eq(*id),
                                    items::score.eq(*score),
                                    items::name.eq(Some(*name)),
                                    items::code.eq(Some(*code)),
                                ))
                                .execute(&mut self.connection),
                        )
                    }
                    Step::InsertItemWithShortName { id, score, name, short_name } => {
                        Outcome::of(
                            &diesel::insert_into(items::table)
                                .values((
                                    items::id.eq(*id),
                                    items::score.eq(*score),
                                    items::name.eq(Some(*name)),
                                    items::short_name.eq(Some(*short_name)),
                                ))
                                .execute(&mut self.connection),
                        )
                    }
                    Step::InsertItemWithDoubled { id, score, name, doubled_score } => {
                        Outcome::of(
                            &diesel::insert_into(items::table)
                                .values((
                                    items::id.eq(*id),
                                    items::score.eq(*score),
                                    items::name.eq(Some(*name)),
                                    items::doubled_score.eq(*doubled_score),
                                ))
                                .execute(&mut self.connection),
                        )
                    }
                    Step::InsertItemWithCascadeId { id, score, name, cascade_id } => {
                        Outcome::of(
                            &diesel::insert_into(items::table)
                                .values((
                                    items::id.eq(*id),
                                    items::score.eq(*score),
                                    items::name.eq(Some(*name)),
                                    items::cascade_id.eq(Some(*cascade_id)),
                                ))
                                .execute(&mut self.connection),
                        )
                    }
                    Step::InsertItemWithRestrictId { id, score, name, restrict_id } => {
                        Outcome::of(
                            &diesel::insert_into(items::table)
                                .values((
                                    items::id.eq(*id),
                                    items::score.eq(*score),
                                    items::name.eq(Some(*name)),
                                    items::restrict_id.eq(Some(*restrict_id)),
                                ))
                                .execute(&mut self.connection),
                        )
                    }
                    Step::DeleteParentCascade { id } => {
                        Outcome::of(
                            &diesel::delete(
                                parent_cascade::table.filter(parent_cascade::id.eq(*id)),
                            )
                            .execute(&mut self.connection),
                        )
                    }
                    Step::DeleteParentRestrict { id } => {
                        Outcome::of(
                            &diesel::delete(
                                parent_restrict::table.filter(parent_restrict::id.eq(*id)),
                            )
                            .execute(&mut self.connection),
                        )
                    }
                    Step::CountItems => {
                        let n: i64 = items::table
                            .count()
                            .get_result(&mut self.connection)
                            .expect("count items");
                        Outcome::Count(n)
                    }
                    Step::CountItemsWhereStatusPending { id } => {
                        let n: i64 = items::table
                            .filter(items::id.eq(*id).and(items::status.eq(Some("pending"))))
                            .count()
                            .get_result(&mut self.connection)
                            .expect("count items where status = pending");
                        Outcome::Count(n)
                    }
                    Step::CountItemsWhereCreatedAtSet { id } => {
                        let n: i64 = items::table
                            .filter(items::id.eq(*id).and(items::created_at.is_not_null()))
                            .count()
                            .get_result(&mut self.connection)
                            .expect("count items where created_at is not null");
                        Outcome::Count(n)
                    }
                    Step::CountItemsWhereDoubledScore { id, doubled } => {
                        let n: i64 = items::table
                            .filter(items::id.eq(*id).and(items::doubled_score.eq(*doubled)))
                            .count()
                            .get_result(&mut self.connection)
                            .expect("count items where doubled_score matches");
                        Outcome::Count(n)
                    }
                    Step::CountItemsWhereCascadeId { cascade_id } => {
                        let n: i64 = items::table
                            .filter(items::cascade_id.eq(Some(*cascade_id)))
                            .count()
                            .get_result(&mut self.connection)
                            .expect("count items where cascade_id matches");
                        Outcome::Count(n)
                    }
                    Step::CountParentRestrict => {
                        let n: i64 = parent_restrict::table
                            .count()
                            .get_result(&mut self.connection)
                            .expect("count parent_restrict");
                        Outcome::Count(n)
                    }
                }
            }
        }
    };
}

runner!(PgRunner, PgConnection);
runner!(SqliteRunner, SqliteConnection);

fn scenario() -> Vec<Step> {
    vec![
        // Seed both FK parents.
        Step::InsertParentCascade { id: 1, label: "p1" },
        Step::InsertParentCascade { id: 2, label: "p2" },
        Step::InsertParentRestrict { id: 1, label: "r1" },
        Step::InsertParentRestrict { id: 2, label: "r2" },
        // CHECK: score in [0, 100] is admitted.
        Step::InsertItem { id: 1, score: 50, name: "alice" },
        // CHECK: score below zero is forbidden.
        Step::InsertItem { id: 2, score: -1, name: "bob" },
        // Only the admitted row lands.
        Step::CountItems,
        // NOT NULL: a non-null name is admitted.
        Step::InsertItem { id: 3, score: 30, name: "charlie" },
        // NOT NULL: omitting name (no default) is forbidden.
        Step::InsertItemWithoutName { id: 4, score: 40 },
        Step::CountItems,
        // UNIQUE: a fresh code is admitted.
        Step::InsertItemWithCode { id: 5, score: 20, name: "dave", code: "X1" },
        // UNIQUE: the same code is forbidden.
        Step::InsertItemWithCode { id: 6, score: 25, name: "eve", code: "X1" },
        Step::CountItems,
        // VARCHAR(10): exactly ten characters is admitted.
        Step::InsertItemWithShortName { id: 7, score: 10, name: "fred", short_name: "abcdefghij" },
        // VARCHAR(10): eleven characters is forbidden.
        Step::InsertItemWithShortName {
            id: 8,
            score: 15,
            name: "grace",
            short_name: "abcdefghijk",
        },
        Step::CountItems,
        // Literal default: both engines default status to 'pending'.
        Step::InsertItem { id: 9, score: 5, name: "henry" },
        Step::CountItemsWhereStatusPending { id: 9 },
        // Function default: now() / datetime('now') leaves created_at non-null.
        Step::CountItemsWhereCreatedAtSet { id: 9 },
        // Generated column: score = 21 must produce doubled_score = 42.
        Step::InsertItem { id: 10, score: 21, name: "iris" },
        Step::CountItemsWhereDoubledScore { id: 10, doubled: 42 },
        // Writing a value into a generated column is forbidden.
        Step::InsertItemWithDoubled { id: 11, score: 30, name: "jack", doubled_score: 100 },
        // Row count is unchanged after the refused insert.
        Step::CountItems,
        // CASCADE FK: child disappears when the parent is deleted.
        Step::InsertItemWithCascadeId { id: 12, score: 5, name: "kate", cascade_id: 1 },
        Step::CountItemsWhereCascadeId { cascade_id: 1 },
        Step::DeleteParentCascade { id: 1 },
        Step::CountItemsWhereCascadeId { cascade_id: 1 },
        // RESTRICT FK: deleting the parent is forbidden while a child exists.
        Step::InsertItemWithRestrictId { id: 13, score: 5, name: "leo", restrict_id: 1 },
        Step::DeleteParentRestrict { id: 1 },
        // Both parent rows survive the refused delete.
        Step::CountParentRestrict,
    ]
}

fn pg_runner() -> PgRunner {
    let mut connection = postgres_harness::fresh_database();
    postgres_harness::apply(&mut connection, SOURCE)
        .unwrap_or_else(|e| panic!("apply source to pg: {e}"));
    PgRunner { connection }
}

fn sqlite_runner() -> SqliteRunner {
    let translated = Pg2Sqlite::default()
        .sql(SOURCE)
        .expect("parse source")
        .translate(&options())
        .expect("translate source");

    let mut connection = establish_connection();
    for stmt in &translated {
        // DDL migration: raw SQL is the correct form here.
        diesel::sql_query(stmt.to_string())
            .execute(&mut connection)
            .unwrap_or_else(|e| panic!("DDL failed: {e}\n{stmt}"));
    }
    SqliteRunner { connection }
}

/// Every constraint is driven with an admitted write and a forbidden write.
/// The storage is compared after each. A disagreement is a finding about
/// the translation.
#[test]
fn constraint_and_default_engines_agree() {
    let mut pg = pg_runner();
    let mut sq = sqlite_runner();

    for step in scenario() {
        let expected = pg.step(&step);
        let actual = sq.step(&step);
        assert_eq!(actual, expected, "engines disagree on {step:?}");
    }
}
