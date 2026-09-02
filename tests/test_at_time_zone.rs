//! `AT TIME ZONE`, which is two operations sharing one syntax.
//!
//! Measured on PostgreSQL 16 with the session zone at UTC, over the wall clock
//! `2023-01-15 12:00:00`:
//!
//! | expression | answer | shift |
//! |---|---|---|
//! | `TIMESTAMP ... AT TIME ZONE '+05:30'` | 17:30 | plus |
//! | `TIMESTAMPTZ ... AT TIME ZONE '+05:30'` | 06:30 | minus |
//! | `TIMESTAMP ... AT TIME ZONE 'UTC'` | 12:00 | none |
//! | `TIMESTAMPTZ ... AT TIME ZONE 'UTC'` | 12:00 | none |
//! | `TIMESTAMP ... AT TIME ZONE 'utc+02:30'` | 14:30 | plus |
//!
//! The plus on the first row is not a typo. PostgreSQL reads a bare `'+05:30'`
//! STRING as a POSIX zone specification, where the sign is the opposite of the
//! ISO one, so that zone is UTC-5:30 and reading 12:00 as local to it gives
//! 17:30 UTC. `AT TIME ZONE INTERVAL '05:30'` and `AT TIME ZONE 'Asia/Kolkata'`
//! both answer 06:30 instead, and neither spelling reaches SQLite.
//!
//! SQLite's `'utc'` modifier is not a no-op either: it reads the value as local
//! time and converts, so it shifts by whatever offset the machine running the
//! query happens to have.

#[path = "helpers/run_translated.rs"]
mod run_translated_helper;

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use run_translated_helper::run_translated_with;
use sqlparser::{dialect::PostgreSqlDialect, parser::Parser};

const TABLE: &str = "CREATE TABLE t (id INT PRIMARY KEY, naive TIMESTAMP, aware TIMESTAMPTZ);
     INSERT INTO t VALUES (1, '2023-01-15 12:00:00', '2023-01-15 12:00:00');";

fn evaluate(expression: &str) -> Option<String> {
    run_translated_with(
        &format!("{TABLE} SELECT {expression} FROM t;"),
        &Pg2SqliteOptions::default(),
    )
    .into_iter()
    .next()
    .expect("one row")
}

fn refuse(expression: &str) -> String {
    Pg2Sqlite::default()
        .sql(&format!("{TABLE} SELECT {expression} FROM t;"))
        .expect("parse")
        .translate(&Pg2SqliteOptions::default())
        .expect_err("this operand has no determinable direction")
        .to_string()
}

/// Reverse translates `sqlite` against the schema and returns the refusal.
fn refuse_reverse(sqlite: &str) -> String {
    let translator = Pg2Sqlite::default().sql(TABLE).expect("parse schema");
    let schema = translator.build_schema().expect("schema");
    translator
        .reverse_sql(sqlite, &schema, &Pg2SqliteOptions::default())
        .expect_err("an operand of unknown type has no determinable zone")
        .to_string()
}

/// Forward translates a whole script and returns its last emitted statement.
fn translate_ok(pg: &str) -> String {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("translate")
        .pop()
        .expect("at least one statement")
}

/// Forward translates `SELECT <expression> FROM t` and reverses the emitted
/// SELECT back to PostgreSQL.
fn reverse_of(expression: &str) -> sqlparser::ast::Statement {
    let emitted = Pg2Sqlite::default()
        .sql(&format!("{TABLE} SELECT {expression} FROM t;"))
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("translate")
        .pop()
        .expect("the SELECT");

    let translator = Pg2Sqlite::default().sql(TABLE).expect("parse schema");
    let schema = translator.build_schema().expect("schema");
    translator
        .reverse_sql(&emitted, &schema, &Pg2SqliteOptions::default())
        .expect("reverse")
        .into_iter()
        .next()
        .expect("one statement")
}

/// Runs `expression`, then runs the forward translation of its own reversal.
/// Both answers must agree: a reversal that means something else is the defect
/// this catches, and only executing both shows it.
fn round_trip(expression: &str) -> (Option<String>, Option<String>) {
    let forward = evaluate(expression);
    let reversed = reverse_of(expression).to_string();
    let back = run_translated_with(&format!("{TABLE} {reversed};"), &Pg2SqliteOptions::default())
        .into_iter()
        .next()
        .expect("one row");
    (forward, back)
}

/// A naive timestamp is read as local to the zone, so the offset is added.
#[test]
fn a_naive_timestamp_moves_toward_utc() {
    assert_eq!(evaluate("naive AT TIME ZONE '+05:30'"), Some("2023-01-15 17:30:00".to_string()));
    assert_eq!(evaluate("naive AT TIME ZONE '-05:30'"), Some("2023-01-15 06:30:00".to_string()));
}

/// An aware timestamp is already UTC, so converting it into the zone subtracts.
/// This is the direction the translator had backwards.
#[test]
fn an_aware_timestamp_moves_away_from_utc() {
    assert_eq!(evaluate("aware AT TIME ZONE '+05:30'"), Some("2023-01-15 06:30:00".to_string()));
    assert_eq!(evaluate("aware AT TIME ZONE '-05:30'"), Some("2023-01-15 17:30:00".to_string()));
}

/// UTC shifts nothing in PostgreSQL, in either direction. SQLite's `'utc'`
/// modifier shifts by the machine's own offset, so emitting it made the answer
/// depend on where the query ran.
#[test]
fn utc_shifts_nothing() {
    for expression in [
        "naive AT TIME ZONE 'UTC'",
        "aware AT TIME ZONE 'UTC'",
        "naive AT TIME ZONE 'GMT'",
        "naive AT TIME ZONE '+00:00'",
    ] {
        assert_eq!(evaluate(expression), Some("2023-01-15 12:00:00".to_string()), "{expression}");
    }
}

/// The shape a schema actually writes, and the one a consumer reported: a
/// column `DEFAULT` over a function rather than a projection over a column.
/// Neither the position nor the operand kind was covered before.
///
/// The three defaults are compared against each other inside one `INSERT`,
/// because SQLite computes the current time once per statement, so the
/// comparison is exact and cannot race the clock. A `'utc'` modifier reaching
/// the output would offset one column from the others by the machine's own
/// offset, which is the whole defect.
///
/// This also pins the `DEFAULT` envelope: SQLite accepts a function call there
/// only in parentheses, so an unwrapped `datetime(...)` would fail to create
/// the table on any machine.
#[test]
fn a_default_over_now_at_utc_stores_the_same_instant_as_a_plain_default() {
    assert_eq!(
        run_translated_with(
            "CREATE TABLE d (
               id INT PRIMARY KEY,
               plain TIMESTAMPTZ DEFAULT now(),
               at_utc TIMESTAMPTZ DEFAULT now() AT TIME ZONE 'UTC',
               stamp_at_utc TIMESTAMPTZ DEFAULT CURRENT_TIMESTAMP AT TIME ZONE 'UTC'
             );
             INSERT INTO d (id) VALUES (1);
             SELECT plain = at_utc AND plain = stamp_at_utc FROM d;",
            &Pg2SqliteOptions::default(),
        ),
        vec![Some("1".to_string())],
    );
}

/// The `utc±HH:MM` spelling carries the same sign as the bare one.
#[test]
fn a_utc_prefixed_offset_reads_the_same_way() {
    assert_eq!(evaluate("naive AT TIME ZONE 'utc+02:30'"), Some("2023-01-15 14:30:00".to_string()));
}

/// Guessing the direction is wrong half the time, so an operand not known to be
/// either kind of timestamp is refused rather than shifted one way and hoped
/// for. A column declared as text lands here too, since its type is known and
/// is not a timestamp.
#[test]
fn an_operand_of_unknown_type_is_refused() {
    let error = refuse("(SELECT max(naive) FROM t) AT TIME ZONE '+05:30'");
    assert!(error.contains("AT TIME ZONE"), "the error must name the construct, got: {error}");
}

/// A named zone still has no SQLite equivalent.
#[test]
fn a_named_zone_is_refused() {
    let error = refuse("naive AT TIME ZONE 'America/New_York'");
    assert!(error.contains("AT TIME ZONE"), "got: {error}");
}

/// An aware operand's offset is negated on the way out, so the reverse
/// direction has to negate it back. Emitting the stored sign turns 06:30 into
/// 17:30, an eleven hour error that both spellings render without complaint.
#[test]
fn an_aware_offset_survives_the_round_trip() {
    let (forward, back) = round_trip("aware AT TIME ZONE '+05:30'");
    assert_eq!(forward, Some("2023-01-15 06:30:00".to_string()));
    assert_eq!(back, forward, "the round trip moved the instant");
}

/// The naive direction keeps its sign, which is the half that already worked.
/// This guards the fix from inverting it.
#[test]
fn a_naive_offset_survives_the_round_trip() {
    let (forward, back) = round_trip("naive AT TIME ZONE '+05:30'");
    assert_eq!(forward, Some("2023-01-15 17:30:00".to_string()));
    assert_eq!(back, forward, "the round trip moved the instant");
}

/// Neither side of a UTC round trip may move the instant. This is the value
/// half of the guarantee, and it holds on any machine because both spellings
/// answer the stored wall clock rather than a converted one.
#[test]
fn utc_survives_the_round_trip() {
    for expression in ["naive AT TIME ZONE 'UTC'", "aware AT TIME ZONE 'UTC'"] {
        let (forward, back) = round_trip(expression);
        assert_eq!(forward, Some("2023-01-15 12:00:00".to_string()), "{expression}");
        assert_eq!(back, forward, "the round trip moved the instant: {expression}");
    }
}

/// The value tests above cannot see this one: pg2sqlite's own forward direction
/// accepts `datetime`, so a reversal that hands the SQLite call straight back
/// still round trips through SQLite. PostgreSQL has no `datetime` function at
/// all, measured on 16: `function datetime(unknown) does not exist`. So the
/// reversal is compared as a tree against the PostgreSQL it claims to be.
#[test]
fn the_reversal_is_the_expression_it_came_from() {
    for expression in [
        "naive AT TIME ZONE '+05:30'",
        "aware AT TIME ZONE '+05:30'",
        "naive AT TIME ZONE 'UTC'",
        "aware AT TIME ZONE 'UTC'",
        // Nested, because sqlparser's Display adds no parentheses and PostgreSQL
        // reads AT TIME ZONE left to right, so the reversal has to come back
        // associating the same way it went out.
        "naive AT TIME ZONE 'UTC' AT TIME ZONE 'UTC'",
    ] {
        let expected =
            Parser::parse_sql(&PostgreSqlDialect {}, &format!("SELECT {expression} FROM t"))
                .expect("the expected PostgreSQL parses")
                .pop()
                .expect("one statement");
        assert_eq!(reverse_of(expression), expected, "reversing {expression}");
    }
}

/// The reported expression's own reversal, which nothing else covered. Its
/// operand is a function rather than a column, so the awareness that decides
/// the sign is read off the function name and the schema is never consulted,
/// and `now()` is aware, so the offset case exercises the flip back on a path
/// with no column in it at all.
///
/// `now()` returns as `NOW()`. The forward direction lowers it to
/// `datetime('now')` and the reverse direction spells the restored call in
/// upper case, so this round trip is a fixed point in meaning and not in
/// spelling. That predates this work and is left alone, which is why the
/// expected form is written out rather than reusing the input.
#[test]
fn a_function_operand_reverses_with_its_sign_restored() {
    for (expression, expected) in [
        ("now() AT TIME ZONE 'UTC'", "NOW() AT TIME ZONE 'UTC'"),
        ("now() AT TIME ZONE '+05:30'", "NOW() AT TIME ZONE '+05:30'"),
        ("CURRENT_TIMESTAMP AT TIME ZONE 'UTC'", "CURRENT_TIMESTAMP AT TIME ZONE 'UTC'"),
    ] {
        let parsed = Parser::parse_sql(&PostgreSqlDialect {}, &format!("SELECT {expected} FROM t"))
            .expect("the expected PostgreSQL parses")
            .pop()
            .expect("one statement");
        assert_eq!(reverse_of(expression), parsed, "reversing {expression}");
    }
}

/// An operand whose type cannot be resolved is refused, and the message names
/// the reference and how to make it resolvable. The reference is now refused
/// where it is read rather than where the offset is applied, because resolution
/// runs against the relations in scope: `mystery` is declared by nothing, so
/// the sign question is never reached.
#[test]
fn reversing_an_offset_over_an_unresolvable_operand_is_refused() {
    let error = refuse_reverse("SELECT datetime(mystery, '+05:30') FROM t");
    assert!(error.contains("mystery"), "the error must name the reference, got: {error}");
    assert!(
        error.contains("cannot be resolved to a declared column"),
        "the unresolved reference is the reason here, got: {error}"
    );
}

/// PostgreSQL applies `AT TIME ZONE` only to a timestamp, so a zone carrying no
/// sign at all still needs the operand's type. Emitting these anyway answered
/// `function pg_catalog.timezone(unknown, text) does not exist`, measured on 16
/// over a column declared `TEXT`.
///
/// Inverted from `reversing_a_zero_offset_needs_no_type`, which asserted the
/// zero offset was exempt. It is not, and the exemption was the R100 gap.
#[test]
fn reversing_a_zone_that_carries_no_sign_still_needs_the_operand_type() {
    for sqlite in [
        "SELECT datetime(mystery) FROM t",
        "SELECT datetime(mystery, 'utc') FROM t",
        "SELECT datetime(mystery, 'localtime') FROM t",
        "SELECT datetime(mystery, '+00:00') FROM t",
    ] {
        let error = refuse_reverse(sqlite);
        assert!(
            error.contains("cannot be resolved to a declared column"),
            "an operand no relation declares is refused, got: {error} for {sqlite}"
        );
    }
}

/// The forward direction deliberately does NOT take this rule, and the reason
/// is the two type systems rather than an oversight. SQLite is dynamically
/// typed, so `datetime(txt, '+05:30')` over a text column answers `2023-01-15
/// 17:30:00` rather than complaining, measured. PostgreSQL is not, so only the
/// reverse direction has to establish the type. The forward direction keeps its
/// own refusal for the sign, which is a different question.
#[test]
fn the_forward_direction_still_accepts_a_text_operand_for_a_zone_with_no_sign() {
    let sqlite = translate_ok(
        "CREATE TABLE d (id INT PRIMARY KEY, txt TEXT);
         SELECT txt AT TIME ZONE 'UTC' FROM d;",
    );
    assert_eq!(sqlite, "SELECT datetime(txt) FROM d");
}
