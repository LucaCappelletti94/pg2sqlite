# pg2sqlite fuzz harness

Four `libFuzzer` targets run via [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz). Each decodes `arbitrary` input into `Pg2SqliteOptions` followed by SQL text.

- `fuzz_sql_translation` feeds the SQL through `Pg2Sqlite::sql().translate()`.
- `fuzz_translate_apply` translates the SQL and applies the result to an in-memory SQLite database, failing on a syntax-class SQLite error.
- `fuzz_reverse_translation` feeds the SQL through `reverse_sql()` against a fixed schema (users, posts, tags, items, covering JSONB, UUID, and vector(128)) and requires the output to parse as PostgreSQL. The schema is parsed once on first iteration via `LazyLock` so each step only pays for `reverse_sql` itself.
- `fuzz_roundtrip_equivalence` translates forward, reverses against the same schema, translates forward again, and requires both SQLite renders to match.

The parse-only targets cap the SQL at 500 bytes, the apply and round-trip targets at 1024.

## Run

```
cargo install --locked cargo-fuzz
cargo fuzz run fuzz_sql_translation
cargo fuzz run fuzz_reverse_translation
```

Append `-- -max_total_time=60` (or similar libfuzzer flags) to time-cap a session. Crashes land in `fuzz/artifacts/<target>/<hash>`. Reproduce a single artefact with `cargo fuzz run <target> fuzz/artifacts/<target>/<hash>`.

## Seed corpus

Every target has structured seeds in `fuzz/seeds/<target>/`, and the ClusterFuzzLite build fails for a target without seeds. The corpus includes encoded test inputs and fuzzer-grown inputs, reduced with edge-only set cover. Pass the seeds as a second corpus directory for a local run.

```
mkdir -p fuzz/corpus/fuzz_sql_translation
cargo fuzz run fuzz_sql_translation fuzz/corpus/fuzz_sql_translation fuzz/seeds/fuzz_sql_translation
```

To refresh a target's seeds after a longer run, reduce the grown corpus into an empty directory and replace `fuzz/seeds/<target>/` with it.

```
mkdir -p /tmp/seeds
cargo fuzz run fuzz_sql_translation /tmp/seeds fuzz/corpus/fuzz_sql_translation fuzz/seeds/fuzz_sql_translation -- -set_cover_merge=1 -use_counters=0
```

## Run every target in parallel

`fuzz/run-all.sh` spawns a tmux session (`pg2sqlite-fuzz`) with one tiled pane per target, discovered dynamically via `cargo fuzz list`. Re-invoking while the session is alive attaches instead of restarting fuzzing.

```
fuzz/run-all.sh                          # start (or attach to) the session
fuzz/run-all.sh -- -max_total_time=600   # extra args appended to defaults
fuzz/run-all.sh --kill                   # tear the session down
```

Defaults passed to every libFuzzer instance (overridable via `--`):

| flag | value | reason |
|------|-------|--------|
| `-timeout=15` | 15 s | abort a single input that runs longer (defense in depth against new sqlparser exponentials). |
| `-max_len=65536` | 64 KiB | cap generated input size, so the artefact directory does not fill with low-signal multi-MB cases. |
| `-rss_limit_mb=8192` | 8 GiB | raise libFuzzer's 2 GiB RSS ceiling. ASAN's allocator fragments and total RSS drifts past 2 GiB after tens of thousands of iterations. The previous OOM artefacts (`35c504`, `0c50`) all replayed in <100 ms / <50 MB on a fresh process, attributable to this drift. |

## Coverage

```
cargo fuzz coverage fuzz_sql_translation
cargo cov -- show target/x86_64-unknown-linux-gnu/coverage/x86_64-unknown-linux-gnu/release/fuzz_sql_translation --instr-profile=fuzz/coverage/fuzz_sql_translation/coverage.profdata
```

See `cargo fuzz --help` for the full subcommand list.
