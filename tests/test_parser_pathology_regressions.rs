//! Regression locks for sqlparser-rs exponential-parse pathologies that
//! our fuzz harness has surfaced.
//!
//! Every test feeds a known-pathological input through the parser
//! (forward) or `reverse_sql` (reverse) and asserts the call completes
//! inside a 500 ms wall budget. The suite was authored RED against pin
//! `b14285d1` (branch `visitor-wasm`); the live pin
//! `306abb569fcb64b84c79ab2284c71d69a4854307` (branch
//! `pathological-combined`) bundles six exponential-parse fixes and
//! now drives the whole suite GREEN. Tracking:
//!
//! - PR #2344 (compound chains, MERGED upstream)        - included via the pin.
//! - PR #2349 (named-arg chains, OPEN upstream)         - included via the pin.
//! - PR #2350 (compound keyword chains, OPEN)           - included via the pin.
//!   The compound-keyword fix turned out to also cover the `IF(...)` chain that
//!   the forward fuzz artefacts hit, so no separate IF-keyword PR was needed.
//! - speculative `NOT`-prefix parsing fix (a741dbdd, branch `pathological4`) -
//!   included via the pin. Fixes the `current_time(...current_time(...))` OOM
//!   case the fuzzer surfaced on the previous bump.
//! - speculative prefix parsing fix (originally df973297, superseded on the
//!   fork by the unified `pathological6` cache `7dcc6117`) - included via the
//!   pin. Fixes the `IF` + reserved-keyword-call exponential (PG
//!   `if(current_time(...))` and SQLite `If-current_time(...)` both bounded).
//! - CASE-arm extension of the same `failed_prefix_positions` cache (`306abb56`
//!   on `pathological-combined`) - included via the pin. Fixes the
//!   `If-c=case<TAB>-...` exponential the fuzz harness found after the
//!   function-call branch was closed.
//!
//! The parse work runs on a worker thread; if it does not signal back
//! within the budget, the assertion fires immediately so cargo test
//! does not block for the full pathological run. The worker keeps
//! running but is bounded by the test binary's process lifetime.

use std::{sync::mpsc, thread, time::Duration};

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

// 500 ms keeps a wide margin above any non-pathological parse (the
// slowest fixture lands at ~120 ms in release under heavy suite
// parallelism) while still firing immediately on an exponential
// regression, which manifests as multi-second to multi-minute work.
const PARSE_BUDGET: Duration = Duration::from_millis(500);

const REVERSE_SCHEMA: &str = "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT);";

fn assert_forward_under_budget(label: &str, sql: String) {
    let (tx, rx) = mpsc::channel();
    thread::Builder::new()
        .name(format!("parse-forward-{label}"))
        .spawn(move || {
            let _ = Pg2Sqlite::default().sql(&sql);
            let _ = tx.send(());
        })
        .expect("spawn worker");

    assert!(
        rx.recv_timeout(PARSE_BUDGET).is_ok(),
        "[{label}] forward parse exceeded {PARSE_BUDGET:?} - sqlparser pin still hits the \
         exponential-parse path for this input. Update the [patch.crates-io] sqlparser rev \
         once the matching upstream PR has merged."
    );
}

fn assert_reverse_under_budget(label: &str, sqlite_sql: String) {
    let (tx, rx) = mpsc::channel();
    thread::Builder::new()
        .name(format!("reverse-sql-{label}"))
        .spawn(move || {
            let translator = Pg2Sqlite::default().sql(REVERSE_SCHEMA).expect("schema parse");
            let schema = translator.build_schema().expect("schema build");
            let opts = Pg2SqliteOptions::default();
            let _ = translator.reverse_sql(&sqlite_sql, &schema, &opts);
            let _ = tx.send(());
        })
        .expect("spawn worker");

    assert!(
        rx.recv_timeout(PARSE_BUDGET).is_ok(),
        "[{label}] reverse_sql exceeded {PARSE_BUDGET:?} - sqlparser pin still hits the \
         exponential-parse path for this input. Update the [patch.crates-io] sqlparser rev \
         once the matching upstream PR has merged."
    );
}

// --------------------------------------------------------------------
// Synthetic minimal reproducer for the IF-keyword chain pathology.
//
// `if(if(if(...x` exhibits ~2x growth in parse time per added `if(`,
// while plain `f(f(...))` and `@(@(...))` stay flat. Depth 25 sits at
// ~10 s in the unfixed parser; depth 20 is ~574 ms. Either depth is
// well over the 100 ms budget today.
// --------------------------------------------------------------------

#[test]
fn forward_if_keyword_chain_depth_25_under_budget() {
    let sql = "if(".repeat(25) + "x";
    assert_forward_under_budget("if_chain_depth_25", sql);
}

// Minimal synthetic of the c1f1 OOM family: `if(` followed by N
// nested `current_time(` calls. The leading `if(` is essential -
// dropping it, the same chain parses in ~100 us at any depth.
// Empirical growth on pin `f63e42fe`:
//
//   depth 10 (134 B):    2.2 ms
//   depth 15 (199 B):     50 ms
//   depth 20 (264 B):    1.5 s    <- this test
//   depth 25 (329 B):     51 s
//
// Roughly 2.7x per +1 nested call - textbook exponential. Depth 20
// is well over the 500 ms budget so the test goes RED today and
// turns GREEN once the upstream parser path is fixed (the chain
// should drop into single-digit milliseconds at any depth).
#[test]
fn forward_if_current_time_chain_depth_20_under_budget() {
    let sql = String::from("if(") + &"current_time(".repeat(20) + "x";
    assert_forward_under_budget("if_current_time_chain_depth_20", sql);
}

// SQLite-dialect manifestation of the same `IF` + reserved-keyword-
// call exponential: scaling under `reverse_sql` is identical
// (~2.7x per +1 nested call), confirming the pathology lives in the
// dialect-shared parser path. Two fuzz artefacts found by the
// reverse target after the `pathological-combined` bump while the
// upstream fix was still in flight:
//
// - `reverse_oom_if_current_time_abd9` (336 B, 105 ms release, ~10 MB RSS;
//   libfuzzer flagged OOM under instrumentation).
// - `reverse_slow_if_current_time_9c5f` (451 B, 1.48 s release).
//
// Both stay #[ignore]'d alongside the forward synthetic and turn
// GREEN once the same upstream fix lands.
#[test]
fn reverse_if_current_time_chain_synthetic_depth_20_under_budget() {
    let sqlite_sql = String::from("If-") + &"current_time(".repeat(20) + "x";
    assert_reverse_under_budget("reverse_if_current_time_depth_20", sqlite_sql);
}

// Sibling of `forward_fuzz_artefact_oom_bracket_subscript_c1f1`: the
// release-side path takes ~105 ms and ~10 MB RSS, bounded under the
// 500 ms budget today. libfuzzer's instrumented run amplified it past
// rss_limit_mb during a sustained session. Keep it non-ignored so a
// future pin regression that re-introduces the exponential blow-up
// here trips it immediately.
#[test]
fn reverse_fuzz_artefact_oom_if_current_time_abd9() {
    let sqlite_sql =
        include_str!("fixtures/parser_pathology/reverse_oom_if_current_time_abd9.bin").to_string();
    assert_reverse_under_budget("reverse_oom_if_current_time_abd9", sqlite_sql);
}

#[test]
fn reverse_fuzz_artefact_slow_if_current_time_9c5f() {
    let sqlite_sql =
        include_str!("fixtures/parser_pathology/reverse_slow_if_current_time_9c5f.bin").to_string();
    assert_reverse_under_budget("reverse_slow_if_current_time_9c5f", sqlite_sql);
}

// Second branch of the `If` speculative-prefix family that
// `df973297` did NOT cover: `If-` followed by chained `c=case<TAB>-`
// (the `CASE` keyword in an assignment-like position). Scaling is
// identical to the `current_time(` chain - ~2.7x per added segment:
//
//   depth 10  (84 B):  2.9 ms
//   depth 15 (124 B):  64 ms
//   depth 20 (164 B):  1.94 s    <- this synthetic
//   depth 25 (204 B):  30.7 s
//   depth 30 (244 B):  72 s
//
// Stripping the leading `If-` makes the same `c=case` chain flat at
// ~500 us at any depth, so the trigger is the same speculative-prefix
// descent into a different keyword-expression form (CASE rather than
// IF-call).
#[test]
fn reverse_if_case_chain_synthetic_depth_20_under_budget() {
    let sqlite_sql = String::from("If-") + &"c=case\t-".repeat(20) + "c";
    assert_reverse_under_budget("reverse_if_case_chain_depth_20", sqlite_sql);
}

#[test]
fn reverse_fuzz_artefact_timeout_if_case_2e21() {
    let sqlite_sql =
        include_str!("fixtures/parser_pathology/reverse_timeout_if_case_2e21.bin").to_string();
    assert_reverse_under_budget("reverse_timeout_if_case_2e21", sqlite_sql);
}

#[test]
fn reverse_fuzz_artefact_slow_if_case_dff9() {
    let sqlite_sql =
        include_str!("fixtures/parser_pathology/reverse_slow_if_case_dff9.bin").to_string();
    assert_reverse_under_budget("reverse_slow_if_case_dff9", sqlite_sql);
}

#[test]
fn reverse_fuzz_artefact_slow_if_case_db47() {
    let sqlite_sql =
        include_str!("fixtures/parser_pathology/reverse_slow_if_case_db47.bin").to_string();
    assert_reverse_under_budget("reverse_slow_if_case_db47", sqlite_sql);
}

// --------------------------------------------------------------------
// Forward fuzz artefacts. Both inputs hit the `IF(...)` exponential
// path through the postgres dialect; `b624` is the smallest specimen
// the fuzzer found (~29 s today), `c01cb` is a larger variant that
// libfuzzer flagged as a hard timeout.
// --------------------------------------------------------------------

#[test]
fn forward_fuzz_artefact_if_chain_b624() {
    let sql = include_str!("fixtures/parser_pathology/forward_if_chain_b624.bin").to_string();
    assert_forward_under_budget("forward_if_chain_b624", sql);
}

#[test]
fn forward_fuzz_artefact_if_chain_c01cb() {
    let sql = include_str!("fixtures/parser_pathology/forward_if_chain_c01cb.bin").to_string();
    assert_forward_under_budget("forward_if_chain_c01cb", sql);
}

// Found by the fuzz harness after the `pathological-combined` bump,
// but only because the running binary had been built against the
// stale `visitor-wasm` pin: libfuzzer flagged it as OOM (>2 GiB RSS
// during parse). Against the new pin the same input parses in ~10 ms
// at ~10 MB RSS. Locking it here so a future pin regression cannot
// reintroduce the exponential-memory path.
#[test]
fn forward_fuzz_artefact_oom_current_time_0c50() {
    let sql = include_str!("fixtures/parser_pathology/forward_oom_0c50.bin").to_string();
    assert_forward_under_budget("forward_oom_0c50", sql);
}

// Second OOM artefact, found post-`pathological4` bump. Differs from
// the 0c50 case: combines `current_time(...)` chains with trailing
// bracket-subscript / colon-slice noise like `puduid[:`, `[Aiuduid[:0`.
// Release parse: ~90 ms at ~7 MB RSS (bounded). Instrumented libfuzzer
// replay: ~2.2 s at ~442 MB RSS - the ~60x sanitizer amplification of
// transient parser allocations is what tripped libfuzzer's default
// 2 GiB rss_limit_mb after accumulating session state. Locking the
// release-side budget here; the upstream fix would tighten parser
// recursion on this combined-pattern path so the per-iteration shadow
// memory shrinks too.
#[test]
fn forward_fuzz_artefact_oom_bracket_subscript_c1f1() {
    let sql = include_str!("fixtures/parser_pathology/forward_oom_c1f1.bin").to_string();
    assert_forward_under_budget("forward_oom_c1f1", sql);
}

// --------------------------------------------------------------------
// Reverse fuzz artefacts. Both inputs hit the compound-identifier /
// compound-keyword chain pathology in the SQLite dialect. Empirically
// validated against the fork's `pathological3` branch (#2349 + #2350
// applied): both drop from seconds to ~100 us once the patches land.
// --------------------------------------------------------------------

#[test]
fn reverse_fuzz_artefact_compound_5e06() {
    let sqlite_sql =
        include_str!("fixtures/parser_pathology/reverse_compound_5e06.bin").to_string();
    assert_reverse_under_budget("reverse_compound_5e06", sqlite_sql);
}

#[test]
fn reverse_fuzz_artefact_compound_d4b8() {
    let sqlite_sql =
        include_str!("fixtures/parser_pathology/reverse_compound_d4b8.bin").to_string();
    assert_reverse_under_budget("reverse_compound_d4b8", sqlite_sql);
}
