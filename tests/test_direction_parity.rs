//! Neither direction may know a name the other does not.
//!
//! `tests/gauntlet/reverse.rs` closes this going back to PostgreSQL, using the
//! server's catalogue as its corpus. Going out to SQLite there is no server to
//! ask, because the corpus is not "what SQLite has" in the abstract but what
//! this crate claims it has, so the corpus is the crate's own inventory and the
//! two sweeps below read it rather than keep a copy.
//!
//! Both are behavioural: they run the translators and look at what comes back,
//! so neither can pass by agreeing with a list.

use pg2sqlite::{
    impls::sqlite_functions::sqlite_names,
    prelude::{Pg2Sqlite, Pg2SqliteOptions, TranslationOptions},
};
use sql_traits::structs::ParserDB;

const DDL: &str = "CREATE TABLE t (id INT PRIMARY KEY, s TEXT, n INT, r REAL, bin BYTEA, \
                   ts TIMESTAMP, payload JSONB);";

/// Every capability declared, since a refusal for want of an opt-in says
/// nothing about whether the two directions agree on a name.
fn options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default().with_math_functions_available()
}

fn schema() -> ParserDB {
    Pg2Sqlite::default()
        .sql(DDL)
        .expect("fixture parses")
        .build_schema()
        .expect("fixture builds a schema")
}

/// The PostgreSQL a SQLite call reverses into, or the refusal.
fn reverse(sqlite: &str) -> Result<String, pg2sqlite::errors::Error> {
    let statements = Pg2Sqlite::default().reverse_sql(sqlite, &schema(), &options())?;
    Ok(statements.iter().map(ToString::to_string).collect::<Vec<_>>().join("; "))
}

fn forward(postgres: &str) -> Result<String, pg2sqlite::errors::Error> {
    let statements =
        Pg2Sqlite::default().sql(&format!("{DDL}{postgres};"))?.translate_to_sql(&options())?;
    Ok(statements.last().cloned().unwrap_or_default())
}

/// The narrowest call shape the reverse direction accepts for `name`, since a
/// fixed arity would measure argument counts rather than names: a one-argument
/// probe refuses `concat_ws` for wanting two. A real argument is preferred over
/// none, because the reverse direction validates nothing about its input and
/// will happily reverse `concat()`.
fn reversible_call(name: &str) -> Option<(String, String)> {
    const ARGUMENTS: [&str; 4] = ["s", "s, s", "s, s, s", ""];
    ARGUMENTS.iter().find_map(|arguments| {
        let sqlite = format!("SELECT {name}({arguments}) FROM t");
        reverse(&sqlite).ok().map(|postgres| (sqlite, postgres))
    })
}

/// Whether a forward refusal is about how many arguments the probe passed
/// rather than about the name.
///
/// The probe picks a call shape that the reverse direction accepts, and the
/// reverse direction accepts any arity, so some shapes are wrong for the
/// function. That is the probe's doing, not a disagreement between directions.
/// It cannot hide one either: a name the forward direction was never taught
/// earns the generic refusal, which says nothing about arguments, so the second
/// clause keeps an omission from ever being filtered out here.
fn is_about_arity(message: &str) -> bool {
    message.contains("argument") && !message.contains("is not a SQLite function")
}

/// What the reverse direction emits, the forward direction has to take back.
///
/// This is the mirror of the omission that prompted all of this. Acceptance is
/// the bar rather than round-trip equality, because the reverse direction
/// legitimately rewrites shape: `total(x)` becomes `COALESCE(SUM(x), 0)` and
/// `hex(x)` becomes `upper(encode(x::BYTEA, 'hex'))`.
#[test]
fn what_the_reverse_direction_emits_translates_back() {
    let mut placed = 0usize;
    let mut broken = Vec::new();

    for name in sqlite_names() {
        let Some((sqlite, postgres)) = reversible_call(name) else {
            continue;
        };
        placed += 1;
        if let Err(error) = forward(postgres.trim_end_matches(';')) {
            let message = error.to_string();
            if !is_about_arity(&message) {
                broken.push(format!("{sqlite}\n    -> {postgres}\n    !! {message}"));
            }
        }
    }

    // A collapse would make the assertion below vacuous. SQLite has 133 names
    // here and the reverse direction places 86 of them.
    assert!(placed >= 70, "the reverse direction should place most SQLite names, got {placed}");

    assert!(
        broken.is_empty(),
        "the reverse direction emits {} thing(s) the forward direction refuses:\n{}",
        broken.len(),
        broken.join("\n")
    );
}

/// The complement, and the gap the scalar sweep in the gauntlet deliberately
/// left: a name **both** engines have, refused going back.
///
/// That sweep excludes those, because whether such a name may cross is a
/// judgement about meaning rather than a fact about existence. This is where
/// the judgement is checked: it has to have been made, and `SQLITE_ONLY` is
/// where the reverse translator records it. A refusal that names itself and
/// gives a reason is a decision. The generic one is an oversight.
#[test]
fn every_sqlite_name_is_placed_or_refused_with_a_reason() {
    let mut unexplained = Vec::new();

    for name in sqlite_names() {
        if reversible_call(name).is_some() {
            continue;
        }
        // Nothing parsed and reversed at any arity, so take the refusal for the
        // one-argument form and ask why.
        let error = reverse(&format!("SELECT {name}(s) FROM t"))
            .expect_err("reversible_call already found no accepted arity");
        let message = error.to_string();
        if message.contains("is not a name this crate knows PostgreSQL has") {
            unexplained.push(format!("{name}: {message}"));
        }
    }

    assert!(
        unexplained.is_empty(),
        "{} SQLite name(s) are refused going back with no reason given:\n{}",
        unexplained.len(),
        unexplained.join("\n")
    );
}
