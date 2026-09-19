//! Membership over a delimited session setting, the only spelling a row level
//! security policy has for "the caller holds this key".
//!
//! `x = ANY(string_to_array(a, d))` and its `<> ALL` negation become an
//! `instr()` search over the delimited text. Every expectation here was
//! measured against PostgreSQL 17.11 before it was written, so a disagreement
//! is a translation defect rather than a guess about PostgreSQL.
//!
//! Each case reads the emitted predicate through a view and a typed diesel
//! query, so the predicate is executed by the same DSL a consumer uses.
//! `batch_execute` applies the emitted script itself, which is DDL the typed
//! DSL cannot express.

use diesel::{connection::SimpleConnection, prelude::*};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, SessionVariableMapping};

diesel::define_sql_function! {
    /// Answers the delimited session setting the policy reads.
    fn current_app_subjects() -> diesel::sql_types::Nullable<diesel::sql_types::Text>;
}

diesel::table! {
    /// The view each corpus case selects through, which carries the emitted
    /// membership predicate.
    held (id) {
        /// The key row the membership test admitted.
        id -> Integer,
    }
}

diesel::table! {
    /// The policy-wrapped view callers read and write through.
    t (owner) {
        /// The share key the row belongs to.
        owner -> Nullable<Text>,
    }
}

diesel::table! {
    /// The backing table the wrapper's triggers write to.
    t_rls (owner) {
        /// The share key the row belongs to.
        owner -> Nullable<Text>,
    }
}

/// Row 4 holds the delimiter, row 5 is NULL and row 6 is empty, the three left
/// sides the rewrite has to answer for.
const KEYS: &str = "CREATE TABLE keys (id INT PRIMARY KEY, k TEXT);
INSERT INTO keys (id, k) VALUES (1, 'a'), (2, 'b'), (3, 'c'), (4, 'a,b'), (5, NULL), (6, '');
";

/// Applies what `pg` translates to, on a connection where the session function
/// answers `subjects`.
///
/// The pragmas are the ones `tests/helpers/mod.rs` gives every other row
/// level security test, so the guarded view is exercised under the settings a
/// replica is opened with rather than under bare defaults.
fn open(pg: &str, options: &Pg2SqliteOptions, subjects: Option<&'static str>) -> SqliteConnection {
    let script = Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate_to_sql(options)
        .expect("translate")
        .iter()
        .map(|statement| format!("{statement};"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut connection = SqliteConnection::establish(":memory:").expect("connect");
    current_app_subjects_utils::register_impl(&mut connection, move || {
        subjects.map(str::to_string)
    })
    .expect("register current_app_subjects");
    connection
        .batch_execute("PRAGMA foreign_keys = ON; PRAGMA recursive_triggers = ON;")
        .expect("set the replica pragmas");
    connection.batch_execute(&script).expect("apply the translated script");
    connection
}

/// The key rows `predicate` admits, read through a typed query over the view
/// the predicate was translated into.
fn admitted(predicate: &str) -> Vec<i32> {
    let pg = format!("{KEYS}CREATE VIEW held AS SELECT id FROM keys WHERE {predicate};");
    let mut connection = open(&pg, &Pg2SqliteOptions::default(), None);
    held::table.select(held::id).order(held::id).load(&mut connection).expect("read the view")
}

fn refusal(pg: &str) -> String {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect_err("translation should be refused")
        .to_string()
}

#[test]
fn membership_admits_exactly_the_delimited_elements() {
    assert_eq!(
        admitted("k = ANY(string_to_array('a,b', ','))"),
        vec![1, 2],
        "PostgreSQL 17.11 answers 1,2 over this corpus"
    );
}

#[test]
fn some_spells_the_same_membership_test() {
    assert_eq!(
        admitted("k = SOME(string_to_array('a,b', ','))"),
        vec![1, 2],
        "SOME is ANY under another name"
    );
}

#[test]
fn the_all_negation_admits_the_complement() {
    assert_eq!(
        admitted("k <> ALL(string_to_array('a,b', ','))"),
        vec![3, 4, 6],
        "PostgreSQL 17.11 answers 3,4,6 over this corpus"
    );
}

/// `string_to_array` never produces an element carrying the delimiter, so
/// PostgreSQL answers false for a left side that carries one. Searching the
/// delimited text alone would find row 4 inside `,x,a,b,y,` and admit a row
/// the server denies.
#[test]
fn a_left_side_carrying_the_delimiter_is_not_an_element() {
    assert_eq!(
        admitted("k = ANY(string_to_array('x,a,b,y', ','))"),
        vec![1, 2],
        "row 4 holds 'a,b', a substring of the setting but not an element of it"
    );
    assert_eq!(
        admitted("k <> ALL(string_to_array('x,a,b,y', ','))"),
        vec![3, 4, 6],
        "the negation admits row 4 for the same reason"
    );
}

/// A NULL left side answers NULL in both forms, which denies the row.
#[test]
fn a_null_left_side_satisfies_neither_form() {
    assert_eq!(
        admitted(
            "k IS NULL AND (k = ANY(string_to_array('a,b', ',')) \
             OR k <> ALL(string_to_array('a,b', ',')))"
        ),
        Vec::<i32>::new(),
        "fail closed: an unset key admits nothing"
    );
}

#[test]
fn an_empty_element_is_matched_by_an_empty_left_side() {
    assert_eq!(
        admitted("k = ANY(string_to_array('a,,b', ','))"),
        vec![1, 2, 6],
        "PostgreSQL 17.11 splits 'a,,b' into a, the empty string, and b"
    );
}

/// The empty text splits into no elements, so every row fails the membership
/// test and every row passes its negation, the NULL key included.
#[test]
fn an_empty_setting_admits_nothing_and_excludes_nothing() {
    assert_eq!(
        admitted("k = ANY(string_to_array('', ','))"),
        Vec::<i32>::new(),
        "PostgreSQL 17.11 splits the empty text into no elements"
    );
    assert_eq!(
        admitted("k <> ALL(string_to_array('', ','))"),
        vec![1, 2, 3, 4, 5, 6],
        "PostgreSQL 17.11 answers true for every left side over an empty array"
    );
}

/// An unset setting answers NULL in both directions, for a left side carrying
/// the delimiter as much as for any other. Reading the delimiter guard outside
/// that NULL would answer false here, and its negation would then expose row 4.
#[test]
fn an_unset_setting_satisfies_neither_form() {
    assert_eq!(
        admitted("k = ANY(string_to_array(NULL, ','))"),
        Vec::<i32>::new(),
        "PostgreSQL 17.11 answers NULL for membership in a NULL array"
    );
    assert_eq!(
        admitted("k <> ALL(string_to_array(NULL, ','))"),
        Vec::<i32>::new(),
        "the negation of NULL is NULL, which admits nothing"
    );
}

const SHARE_KEY_POLICY: &str = "CREATE TABLE t (owner TEXT);
ALTER TABLE t ENABLE ROW LEVEL SECURITY;
CREATE POLICY p ON t USING (owner = ANY(string_to_array(current_setting('app.subjects', true), ',')));
";

fn share_key_options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default().with_rls_audit_table_name("rls_violations").with_session_variable(
        SessionVariableMapping::current_setting("app.subjects", "current_app_subjects"),
    )
}

/// Seeds the backing table the way an authoritative apply does, with the
/// generated triggers disabled.
fn seed(connection: &mut SqliteConnection, owners: &[Option<&str>]) {
    connection.set_triggers_enabled(false).expect("disable triggers for the seed");
    for owner in owners {
        diesel::insert_into(t_rls::table)
            .values(t_rls::owner.eq(*owner))
            .execute(connection)
            .expect("seed the backing table");
    }
    connection.set_triggers_enabled(true).expect("re-enable triggers");
}

fn visible(connection: &mut SqliteConnection) -> Vec<Option<String>> {
    t::table.select(t::owner).order(t::owner).load(connection).expect("read the guarded view")
}

/// The reproduction from the capability report: a policy admitting a share key
/// has to produce a replica that serves the same rows the server does.
#[test]
fn a_share_key_policy_produces_a_filtering_replica() {
    let mut connection = open(SHARE_KEY_POLICY, &share_key_options(), Some("k1,k2"));
    seed(&mut connection, &[Some("k1"), Some("k9"), Some("k1,k2"), None, Some("")]);

    assert_eq!(
        visible(&mut connection),
        vec![Some("k1".to_string())],
        "only the held key is visible"
    );
    let stored: i64 = t_rls::table.count().get_result(&mut connection).expect("count the rows");
    assert_eq!(stored, 5, "the backing table keeps every synced row");
}

/// An unset setting hides every row rather than exposing the table.
#[test]
fn an_unset_session_setting_hides_every_row() {
    let mut connection = open(SHARE_KEY_POLICY, &share_key_options(), None);
    seed(&mut connection, &[Some("k1"), Some("k9")]);

    let count: i64 = t::table.count().get_result(&mut connection).expect("count the view");
    assert_eq!(count, 0, "an unset setting holds no key");
}

/// The same predicate rides the four emitted trigger bodies, where it sits
/// inside `IS NOT TRUE` and beside a second copy of itself, so the write path
/// is executed rather than only applied.
#[test]
fn the_view_write_path_enforces_the_same_membership() {
    let mut connection = open(SHARE_KEY_POLICY, &share_key_options(), Some("k1,k2"));

    diesel::insert_into(t::table)
        .values(t::owner.eq("k1"))
        .execute(&mut connection)
        .expect("a held key may be written through the view");
    assert_eq!(visible(&mut connection), vec![Some("k1".to_string())], "the write is visible");

    let denied = diesel::insert_into(t::table)
        .values(t::owner.eq("k9"))
        .execute(&mut connection)
        .expect_err("a key the caller does not hold must be refused");
    assert!(
        denied.to_string().contains("new row violates row-level security policy"),
        "unexpected error: {denied}"
    );

    let carries_delimiter = diesel::insert_into(t::table)
        .values(t::owner.eq("k1,k2"))
        .execute(&mut connection)
        .expect_err("the whole setting is not one of its own elements");
    assert!(
        carries_delimiter.to_string().contains("new row violates row-level security policy"),
        "unexpected error: {carries_delimiter}"
    );

    let moved_away = diesel::update(t::table.filter(t::owner.eq("k1")))
        .set(t::owner.eq("k9"))
        .execute(&mut connection)
        .expect_err("a row may not be updated out of the caller's reach");
    assert!(
        moved_away.to_string().contains("new row violates row-level security policy"),
        "unexpected error: {moved_away}"
    );

    diesel::update(t::table.filter(t::owner.eq("k1")))
        .set(t::owner.eq("k2"))
        .execute(&mut connection)
        .expect("a held key may become another held key");
    assert_eq!(visible(&mut connection), vec![Some("k2".to_string())], "the update landed");

    seed(&mut connection, &[Some("k9")]);
    diesel::delete(t::table.filter(t::owner.eq("k9")))
        .execute(&mut connection)
        .expect("deleting an invisible row is not an error");
    let stored: i64 = t_rls::table.count().get_result(&mut connection).expect("count the rows");
    assert_eq!(stored, 2, "an invisible row survives the delete, as it does in PostgreSQL");

    diesel::delete(t::table.filter(t::owner.eq("k2")))
        .execute(&mut connection)
        .expect("delete the held row");
    assert!(visible(&mut connection).is_empty(), "the held row is gone");
    let left: i64 = t_rls::table.count().get_result(&mut connection).expect("count the rows");
    assert_eq!(left, 1, "only the row the caller cannot see is left");
}

/// The array itself has no SQLite value, so anything but a membership test
/// over it stays refused.
#[test]
fn a_bare_string_to_array_is_still_refused() {
    let bare = refusal("SELECT string_to_array('a,b', ',');");
    assert!(bare.contains("string_to_array"), "names the function: {bare}");
    let consumed = refusal("SELECT string_to_array('a,b', ',') IS NOT NULL;");
    assert!(consumed.contains("string_to_array"), "names the function: {consumed}");
    let ordered = refusal(
        "CREATE TABLE keys (id INT PRIMARY KEY, k TEXT);\n\
         SELECT id FROM keys WHERE k < ANY(string_to_array('a,b', ','));",
    );
    assert!(
        ordered.contains("string_to_array"),
        "only the equality forms have a membership rewrite: {ordered}"
    );
}

#[test]
fn the_three_argument_form_is_refused() {
    let message =
        refusal("SELECT 'a' = ANY(string_to_array('a,b', ',', 'b')) FROM (SELECT 1) AS s;");
    assert!(message.contains("two-argument"), "names the supported arity: {message}");
}

#[test]
fn a_computed_delimiter_is_refused() {
    let message = refusal(
        "CREATE TABLE d (k TEXT, sep TEXT);\n\
         SELECT k FROM d WHERE k = ANY(string_to_array('a,b', sep));",
    );
    assert!(message.contains("literal"), "names what the delimiter must be: {message}");
}

#[test]
fn an_empty_delimiter_is_refused() {
    let message = refusal("SELECT 'ab' = ANY(string_to_array('ab', '')) FROM (SELECT 1) AS s;");
    assert!(message.contains("literal"), "names what the delimiter must be: {message}");
}

/// A case-insensitive corpus, where `'a'` and `'A'` are one value to the
/// column's own equality and two values to a byte search.
const CASE_INSENSITIVE_KEYS: &str = "CREATE TABLE ci (id INT PRIMARY KEY, k TEXT COLLATE NOCASE);
INSERT INTO ci (id, k) VALUES (1, 'a'), (2, 'A'), (3, 'b');
";

fn case_insensitively_admitted(predicate: &str) -> Vec<i32> {
    let pg =
        format!("{CASE_INSENSITIVE_KEYS}CREATE VIEW held AS SELECT id FROM ci WHERE {predicate};");
    let mut connection = open(&pg, &Pg2SqliteOptions::default(), None);
    held::table.select(held::id).order(held::id).load(&mut connection).expect("read the view")
}

fn case_insensitive_refusal(predicate: &str) -> String {
    refusal(&format!("{CASE_INSENSITIVE_KEYS}SELECT id FROM ci WHERE {predicate};"))
}

/// SQLite's `instr()` compares bytes whatever collation its operands carry.
/// Measured on SQLite 3.51.1, `'a' COLLATE NOCASE = 'A'` answers 1 while
/// `instr(',a,', ',A,')` answers 0, and a column declared `COLLATE NOCASE`
/// compares the same way, so a case-insensitive membership test has no
/// `instr()` form and is refused rather than answered bytewise.
#[test]
fn a_case_insensitive_column_is_refused() {
    let message = case_insensitive_refusal("k = ANY(string_to_array('a,b', ','))");
    assert!(message.contains("NOCASE"), "names the collation: {message}");
}

/// SQLite propagates a column's collation through parentheses and a `CAST`
/// and through nothing else. Measured on SQLite 3.51.1, over a `NOCASE`
/// column `(k) = 'a'` and `CAST(k AS TEXT) = 'a'` answer 1 for `k = 'A'`
/// where `lower(k) = 'A'`, `trim(k) = 'a'` and `k || '' = 'a'` answer 0, so
/// the two that carry the collation are refused with it.
#[test]
fn a_cast_over_a_case_insensitive_column_is_refused() {
    let cast = case_insensitive_refusal("CAST(k AS TEXT) = ANY(string_to_array('a,b', ','))");
    assert!(cast.contains("NOCASE"), "a cast keeps the collation: {cast}");
    let nested = case_insensitive_refusal("(k) <> ALL(string_to_array('a,b', ','))");
    assert!(nested.contains("NOCASE"), "parentheses keep the collation: {nested}");
}

/// SQLite gives a compound select the collation of its leftmost branch,
/// measured on 3.51.1 where a view over `SELECT k FROM ci UNION ALL SELECT
/// 'zz'` answers 1 for `k = 'a'` on the `'A'` row while the search answers 0.
/// The scope reports the two branches as a disagreement, so the rewrite
/// refuses rather than reading the column as byte collated.
#[test]
fn an_unsettled_view_collation_is_refused() {
    let message = refusal(
        "CREATE TABLE ci (id INT PRIMARY KEY, k TEXT COLLATE NOCASE);\n\
         CREATE VIEW v AS SELECT k FROM ci UNION ALL SELECT 'zz';\n\
         SELECT k FROM v WHERE k <> ALL(string_to_array('a', ','));",
    );
    assert!(message.contains("do not settle the collation"), "names what is unsettled: {message}");
}

/// A schema built elsewhere is never revisited by the translation, so a
/// column declared with a collation SQLite has no counterpart for reaches the
/// rewrite with that collation intact. PostgreSQL applies the locale where
/// the search compares bytes, so the rewrite raises the refusal the
/// collation mapping owns rather than assuming bytes for a name it could not
/// map.
#[test]
fn an_unmappable_declared_collation_is_refused() {
    let schema = Pg2Sqlite::default()
        .sql(r#"CREATE TABLE ci (id INT PRIMARY KEY, k TEXT COLLATE "en_US");"#)
        .expect("parse the schema")
        .build_schema()
        .expect("build the schema");
    let error = Pg2Sqlite::default()
        .sql("SELECT k FROM ci WHERE k = ANY(string_to_array('a,b', ','));")
        .expect("parse the query")
        .translate_with_report_and_schema(&schema, &Pg2SqliteOptions::default())
        .expect_err("a locale collation has no byte search")
        .to_string();
    assert!(
        error.contains("EN_US") && error.contains("not a valid SQLite collation"),
        "the mapping's own refusal: {error}"
    );
}

/// `rowid` is SQLite's own column and the scope answers nothing for it by
/// rule, which is not a collation dispute, so the membership test over it
/// keeps translating.
#[test]
fn a_scope_declined_name_still_translates() {
    assert_eq!(
        admitted("CAST(rowid AS TEXT) = ANY(string_to_array('1,2', ','))"),
        vec![1, 2],
        "the first two rows carry rowid 1 and 2"
    );
}

/// A PL/pgSQL body may declare a variable whose name a column also carries,
/// and a qualified reference is still the column, so its declared collation
/// decides. Reading it as bytes emitted a search over a `NOCASE` column.
#[test]
fn a_variable_name_does_not_hide_a_qualified_column_collation() {
    let message = refusal(
        "CREATE TABLE ci (id INT PRIMARY KEY, k TEXT COLLATE NOCASE);\n\
         CREATE TABLE log (id INT PRIMARY KEY, hit BOOLEAN);\n\
         CREATE FUNCTION mark() RETURNS TRIGGER LANGUAGE plpgsql AS $$\n\
         DECLARE k TEXT := 'a';\n\
         BEGIN\n\
           IF EXISTS (SELECT 1 FROM ci WHERE ci.k = ANY(string_to_array(k, ','))) THEN\n\
             RAISE EXCEPTION 'held';\n\
           END IF;\n\
           RETURN NEW;\n\
         END;\n\
         $$;\n\
         CREATE TRIGGER mark_log BEFORE INSERT ON log FOR EACH ROW EXECUTE FUNCTION mark();",
    );
    assert!(message.contains("NOCASE"), "the column's own collation decides: {message}");
}

/// A caller may register a function of their own named `current_setting`,
/// which passes through as the user-defined function it is. Nothing says such
/// a function answers the same value on every call, and the setting is read
/// three times, so only a declared session-variable mapping counts as a
/// stable reading and the rest is refused.
#[test]
fn an_unmapped_session_shaped_function_is_refused() {
    let options = Pg2SqliteOptions::default().with_user_defined_functions(["current_setting"]);
    let error = Pg2Sqlite::default()
        .sql(
            "CREATE TABLE ci (k TEXT);\n\
             SELECT k FROM ci WHERE k = ANY(string_to_array(current_setting('app.subjects'), ','));",
        )
        .expect("parse")
        .translate_to_sql(&options)
        .expect_err("an unmapped session function must not be read three times")
        .to_string();
    assert!(error.contains("more than one place"), "the duplicated-operand refusal: {error}");
}

/// PostgreSQL carries a column's collation through the expressions built
/// over it while SQLite does not, measured on PostgreSQL 17 over a column
/// with a nondeterministic ICU collation holding `'A'`, where `k = 'a'`,
/// `lower(k) = 'A'`, `trim(k) = 'a'` and `k || '' = 'a'` all answer true
/// against 0 from the same expressions in SQLite. A wrapper is therefore no
/// escape from the collation, and the rewrite refuses wherever a collated
/// column is read.
#[test]
fn a_collated_column_inside_a_wrapper_is_refused() {
    for predicate in [
        "lower(k) = ANY(string_to_array('a,b', ','))",
        "trim(k) <> ALL(string_to_array('a,b', ','))",
        "(k || '') = ANY(string_to_array('a,b', ','))",
    ] {
        let message = case_insensitive_refusal(predicate);
        assert!(message.contains("NOCASE"), "{predicate} names the collation: {message}");
    }
}

#[test]
fn an_explicitly_collated_left_side_is_refused() {
    let message = refusal(
        "CREATE TABLE ci (s TEXT);\n\
         SELECT s FROM ci WHERE s COLLATE RTRIM <> ALL(string_to_array('a,b', ','));",
    );
    assert!(message.contains("RTRIM"), "names the collation: {message}");
}

/// A byte collation is what the search already measures, so naming one
/// changes nothing, and an explicit one decides the comparison in both
/// engines even over a column declared otherwise.
#[test]
fn a_binary_collation_still_translates() {
    assert_eq!(
        admitted("k COLLATE BINARY = ANY(string_to_array('a,b', ','))"),
        vec![1, 2],
        "BINARY is the collation instr() compares under"
    );
    assert_eq!(
        case_insensitively_admitted("k COLLATE BINARY = ANY(string_to_array('a,b', ','))"),
        vec![1, 3],
        "the override compares bytes, so 'A' is no element where 'a' and 'b' are"
    );
}

/// Measured on PostgreSQL 17.11: `string_to_array('~', '~~')` answers the one
/// element `~`, so `'' = ANY(...)` over it is false, where a search through
/// `~~~~~` for `~~~~` answers true. A longer delimiter is refused rather than
/// answered wrongly.
#[test]
fn a_multi_character_delimiter_is_refused() {
    let message = refusal("SELECT '' = ANY(string_to_array('~', '~~')) FROM (SELECT 1) AS s;");
    assert!(message.contains("single-character"), "names what the delimiter must be: {message}");
}

/// Both operands are read more than once, the left side for the delimiter
/// guard and the search, the text for the NULL guard, the emptiness test and
/// the search, so an operand that answers differently on each read is
/// refused rather than read twice.
#[test]
fn a_volatile_operand_is_refused_on_either_side() {
    let left = refusal("SELECT c() = ANY(string_to_array('a,b', ',')) FROM (SELECT 1) AS s;");
    assert!(
        left.contains("reads c() once") && left.contains("more than one place"),
        "the refusal is the duplicated-operand one: {left}"
    );
    let text = refusal(
        "CREATE TABLE v (k TEXT);\n\
         SELECT k FROM v WHERE k = ANY(string_to_array(clock_timestamp()::text, ','));",
    );
    assert!(
        text.contains("clock_timestamp") && text.contains("more than one place"),
        "the text side is guarded the same way: {text}"
    );
}

/// A named-argument call is not the shape the rewrite reads, so it keeps the
/// refusal a bare `string_to_array` answers rather than being rewritten from
/// arguments whose order is not positional.
#[test]
fn a_named_argument_call_is_left_refused() {
    let message = refusal(
        "SELECT 'a' = ANY(string_to_array(string => 'a,b', delimiter => ',')) \
         FROM (SELECT 1) AS s;",
    );
    assert!(
        message.contains("not available in standard SQLite"),
        "the bare refusal stands: {message}"
    );
}

/// An explicit collation decides the whole comparison in PostgreSQL, whatever
/// the other operand inherits, so naming a byte collation on the left keeps
/// the search even where the setting's own column carries one, and the rows
/// it admits are the ones a byte comparison admits.
#[test]
fn an_explicit_byte_collation_covers_both_operands() {
    let pg = "CREATE TABLE ci (id INT PRIMARY KEY, k TEXT, setting TEXT COLLATE NOCASE);
INSERT INTO ci (id, k, setting) VALUES (1, 'a', 'a,b'), (2, 'A', 'a,b'), (3, 'b', 'a,b');
CREATE VIEW held AS SELECT id FROM ci WHERE k COLLATE BINARY = ANY(string_to_array(setting, ','));";
    let mut connection = open(pg, &Pg2SqliteOptions::default(), None);
    let admitted: Vec<i32> =
        held::table.select(held::id).order(held::id).load(&mut connection).expect("read the view");
    assert_eq!(
        admitted,
        vec![1, 3],
        "the byte comparison admits 'a' and 'b' and leaves the 'A' row out"
    );
}

/// An explicit collation below a wrapper decides the comparison too, since
/// PostgreSQL derives the wrapper's collation from its argument, so a byte
/// one there keeps the search over a column declared otherwise.
#[test]
fn an_explicit_byte_collation_under_a_wrapper_keeps_the_search() {
    assert_eq!(
        case_insensitively_admitted("lower(k COLLATE BINARY) = ANY(string_to_array('a,b', ','))"),
        vec![1, 2, 3],
        "the fold answers 'a', 'a' and 'b', each an element under a byte comparison"
    );
}

/// PostgreSQL derives a `CASE` collation from its result arms rather than
/// from its condition, so a byte collation named in the condition does not
/// make the comparison bytewise while the arms read a `NOCASE` column.
#[test]
fn a_byte_collation_in_a_condition_does_not_decide() {
    let message = case_insensitive_refusal(
        "CASE WHEN k COLLATE BINARY = 'x' THEN k ELSE k END \
         = ANY(string_to_array('a,b', ','))",
    );
    assert!(message.contains("NOCASE"), "the arms decide: {message}");
}
