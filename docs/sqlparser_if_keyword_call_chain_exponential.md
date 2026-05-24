# Exponential parse time on `IF(...)` wrapping a chain of reserved-keyword function calls

> Status: **FIXED** upstream on the fork's `pathological6` branch (HEAD `7dcc6117`, "Parser: fix exponential parse time on speculative prefix parsing", merged into `pathological-combined` as `306abb56`). The branch generalises the earlier `pathological4` NOT-prefix cache into a single `failed_prefix_positions` BTreeMap that covers both the NOT-prefix and the broader speculative-prefix cases. It closes the function-call branch (PG `if(current_time(...))`, SQLite `If-current_time(...)`) AND the CASE-arm branch (`If-c=case<TAB>-...`) the fuzz harness surfaced as a follow-up. pg2sqlite tracks the fix via its `[patch.crates-io]` sqlparser pin. All regression-lock tests in `tests/test_parser_pathology_regressions.rs` are GREEN.

## Summary

`sqlparser-rs` takes time that grows roughly **2.7x per added nested call** when an `IF`-keyword token is followed by a chain of reserved-keyword function calls such as `current_time(`. At depth 20 the parse already takes ~1.5 s. At depth 25 it takes ~51 s. The same chain of `current_time(` calls **without** the leading `IF` parses in ~100 us at any depth, so the trigger is the speculative parse path the parser takes after committing to `IF` as the start of an `IF`-expression. **Both the PostgreSQL and SQLite dialects are affected**. The pathology lives in the dialect-shared parser path. PG hits it through `if(current_time(...))`. SQLite hits it through `If-current_time(...)` (unary-minus separated). The reverse-side fuzz target found two SQLite-dialect artefacts with identical scaling after the PostgreSQL-side fix was first surfaced.

This pathology is in the same family as the three exponential-parse cases already fixed on the fork's `pathological-combined` branch (compound chains #2344, named-arg chains #2349, compound keyword chains #2350, plus the speculative `NOT`-prefix fix `a741dbdd`), but is not covered by any of them. It was surfaced by the pg2sqlite fuzz harness as a libfuzzer OOM (`fuzz/artifacts/fuzz_sql_translation/oom-c1f1*`) after the `pathological-combined` pin was already in place.

## Minimal reproducer

```rust
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;

let sql = String::from("if(") + &"current_time(".repeat(20) + "x";
let t = std::time::Instant::now();
let _ = Parser::parse_sql(&PostgreSqlDialect {}, &sql);
assert!(
    t.elapsed() < std::time::Duration::from_millis(100),
    "parse took {:?}",
    t.elapsed(),
);
```

Today this assertion fails after ~1.5 s. After the fix the parse should complete in single-digit milliseconds at any depth.

The reproducer is also exposed in pg2sqlite's regression-lock suite as the ignored test `forward_if_current_time_chain_depth_20_under_budget` in `tests/test_parser_pathology_regressions.rs`. Once the upstream fix lands, removing the `#[ignore]` and bumping the `[patch.crates-io]` sqlparser rev is enough to verify the chain.

## Empirical data

Measured on the pg2sqlite fork pin `f63e42fe13b4a8e4b8c943bb83f23f5d397056ba` (branch `pathological-combined`, which contains #2344 + #2349 + #2350 + `a741dbdd`), release build, single-shot `Pg2Sqlite::default().sql(&input)`:

| depth | input size | parse time | growth vs prior step |
|-------|------------|------------|----------------------|
| 10    | 134 B      | 2.2 ms     | -                    |
| 15    | 199 B      | 50 ms      | 23x for +5 calls     |
| 20    | 264 B      | 1.53 s     | 30x for +5 calls     |
| 25    | 329 B      | 51 s       | 33x for +5 calls     |

Per added nested `current_time(` call the time grows by ~2.7x, which is straight exponential. The same chain without `if(` is flat:

| depth | input size | parse time |
|-------|------------|------------|
| 10    | 131 B      | 115 us     |
| 15    | 196 B      | 79 us      |
| 20    | 261 B      | 67 us      |
| 25    | 326 B      | 88 us      |

So the cost is paid only inside the `IF` speculative path, not by `current_time(` chains in general.

### SQLite-dialect manifestation

Measured against `Pg2Sqlite::reverse_sql(...)` (which parses with `SQLiteDialect`), synthetic `If-current_time(current_time(...x`:

| depth | input size | reverse_sql time |
|-------|------------|------------------|
| 10    | 135 B      | 2.7 ms           |
| 15    | 200 B      | 57 ms            |
| 20    | 265 B      | 1.75 s           |
| 25    | 330 B      | 55 s             |

Same ~2.7x-per-added-call growth, confirming the speculative path is dialect-shared rather than PG-specific. Fuzz artefacts:

- `tests/fixtures/parser_pathology/reverse_oom_if_current_time_abd9.bin` (336 B, 105 ms release).
- `tests/fixtures/parser_pathology/reverse_slow_if_current_time_9c5f.bin` (451 B, 1.48 s release).

### Where the OOM came from

The original fuzz artefact (`oom-c1f1952ba684f121950c48f7776db293e27c80ef`, 385 B) parses in ~89 ms / 7 MB peak RSS in a plain release build. Under libfuzzer's instrumented build (coverage + ASAN-style shadow memory) the same input takes ~2.2 s and uses ~442 MB peak RSS, a ~25x time and ~60x memory amplification driven by the deep recursion that the parser walks while exploring the speculative path. On top of accumulated session state (corpus dedup table, edge counters), one iteration was enough to push the running process past libfuzzer's default `rss_limit_mb = 2048`. The synthetic above is the same family expressed cleanly: stripping the fuzz noise leaves the pure `if(` + `current_time(` chain as the trigger.

## Suspected root cause

PostgreSQL has both:

- `IF EXISTS` / `IF NOT EXISTS` keyword forms used by DDL (`DROP TABLE IF EXISTS ...`), and
- the parser-expression treatment of an identifier `if` followed by `(`, which sqlparser tries to parse as a function call.

`current_time` (along with `current_date`, `current_user`, `session_user`, `localtime`, and similar) is a SQL reserved word that the parser treats specially in expression position. It can stand alone, take an optional precision in parens, or appear at the head of `(...)` group expressions during disambiguation.

The likely shape of the pathology: after committing to `IF(` as an expression head, the parser enters a path that, for each inner token starting with a reserved-keyword identifier, tries multiple speculative grammar productions (function call vs. unary prefix vs. reserved-word value), and the speculative work isn't memoised. Each nested level doubles the speculative arms explored, giving the ~2x per level pattern. The fix should look structurally similar to the `pathological4` `NOT`-prefix change: short-circuit the speculation once one production has consumed a `(` and commit to the function-call path.

## Cross-check vs already-fixed cases

The earlier `pathological3` branch (`02ea2d67`, #2349 + #2350) closed the analogous case where the chain head was a compound keyword identifier (`foo.bar.baz` etc.). `pathological4` (`a741dbdd`) closed the speculative `NOT`-prefix case. The `IF`-keyword case shares the same shape (a syntactic head that forces the parser into a multi-arm speculative descent), so the same family of fix is expected to apply.

## Suggested investigation order

1. Run the minimal reproducer above against the current `pathological-combined` HEAD to confirm the ~1.5 s parse at depth 20.
2. Profile (e.g. `cargo flamegraph --example bench_if_current_time -- 20`) to identify which parser arm is being re-entered exponentially.
3. Add a regression test + bench under `tests/parser/postgresql.rs` mirroring the shape of the `pathological4` bench.
4. Land the fix on a `pathological5` branch, merge it into `pathological-combined`, and bump the pg2sqlite pin. The ignored regression test in `tests/test_parser_pathology_regressions.rs` should then pass with `#[ignore]` removed.

## Sibling branch: `IF` + chained `CASE`-keyword expressions

After the `df973297` fix landed, the reverse fuzz target found a structurally similar but distinct branch of the same family. The trigger is no longer a chained function call. It is the `CASE` keyword in an assignment-like position, repeated.

### Minimal reproducer

```rust
use sqlparser::dialect::SQLiteDialect;
use sqlparser::parser::Parser;

let sql = String::from("If-") + &"c=case\t-".repeat(20) + "c";
let t = std::time::Instant::now();
let _ = Parser::parse_sql(&SQLiteDialect {}, &sql);
assert!(
    t.elapsed() < std::time::Duration::from_millis(100),
    "parse took {:?}",
    t.elapsed(),
);
```

### Empirical data

Measured on pin `df973297` against `Pg2Sqlite::reverse_sql(...)` (SQLite dialect), release build:

| depth | input size | reverse_sql time |
|-------|------------|------------------|
| 5     | 44 B       | 0.8 ms           |
| 10    | 84 B       | 2.9 ms           |
| 15    | 124 B      | 64 ms            |
| 20    | 164 B      | 1.94 s           |
| 25    | 204 B      | 30.7 s           |
| 30    | 244 B      | 72 s             |

~2.7x per +1 segment, identical growth profile to the function-call branch. Dropping the leading `If-` makes the same `c=case` chain flat at ~500 us at any depth, so the trigger is again the `If`-speculative descent, but this time descending into a CASE-expression production rather than a function call.

### Fuzz artefacts

- `tests/fixtures/parser_pathology/reverse_timeout_if_case_2e21.bin` (389 B, **66.8 s** release).
- `tests/fixtures/parser_pathology/reverse_slow_if_case_dff9.bin` (415 B, 771 ms).
- `tests/fixtures/parser_pathology/reverse_slow_if_case_db47.bin` (276 B, 976 ms).

### Suspected gap in the `pathological6` cache

`pathological6` (`b6a762e4`) keys its memoisation on `parse_prefix` failures: `failed_prefix_positions: BTreeMap<usize, ParserError>` stores the error per token index, and a subsequent `parse_prefix` at the same index returns the cached error. That mechanism catches the `If-current_time(...)` chain because each `current_time` token starts a `parse_prefix` attempt that ultimately fails at some downstream position, and the cache fires on every second descent.

The CASE-arm chain doesn't appear to flow through the same code path. Most likely one of:

1. The `case` arm is entered through a different function (`parse_case_expression` or `parse_infix` after `=`), so the speculation isn't memoised by `failed_prefix_positions` at all.
2. `parse_prefix` returns `Ok` on the inner `c=case` segments and the doubling happens after the prefix has succeeded, so the failure-keyed cache never gets a chance to short-circuit.

A follow-up generalisation, extending the same memoisation pattern to whichever speculative entry the `c=case` segment actually walks through, or keying on `(start_index, arm)` so successful-then-failed paths are also cached, should close the CASE-arm. The fix is structurally the same as `pathological6`. It just needs to wrap a second speculative entry point.

### Cross-check vs the already-fixed branch

`if(current_time(...))` (PG) and `If-current_time(...)` (SQLite): GREEN on `pathological6` HEAD. `If-c=case<TAB>-c=case<TAB>-...` (SQLite, this section): still RED on the same HEAD (verified: depth 15 = 61 ms, depth 20 = 1.88 s, identical scaling to `pathological-combined`). So the bug is "the `pathological6` cache doesn't intercept the CASE-arm descent", not a general regression of the speculative-prefix machinery.
