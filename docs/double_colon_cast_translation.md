# PostgreSQL `::` casts are emitted verbatim and fail in SQLite

> Status: **FIXED**. The generic cast path now forces the SQLite `CAST(x AS type)` spelling. See `src/impls/translator_impls/expr.rs` (the `Expr::Cast` arm) and `tests/test_cast_operator.rs`.

## Summary

PostgreSQL spells a cast either as `CAST(expr AS type)` or with the `::` operator (`expr::type`). SQLite supports only the `CAST(expr AS type)` form. The translator used to map the target type of a `::` cast correctly but leave the cast rendered as `::`, producing SQL that SQLite rejects at parse time. That broke the crate's guarantee that every emitted statement runs in SQLite.

The `::` cases produced invalid SQLite: `SELECT id::text FROM t` came out as `SELECT id::TEXT FROM t`, which SQLite rejects with `unrecognized token ":"`.

## Root cause

The generic cast arm in `src/impls/translator_impls/expr.rs` cloned the source `kind`:

```rust
Expr::Cast {
    // ...
    kind: kind.clone(), // preserved CastKind::DoubleColon from `::`
    // ...
}
```

`sqlparser` models the cast spelling as `CastKind`. A `::` cast parses as `CastKind::DoubleColon`, and `Display` renders that back as `expr::type`, while `CastKind::Cast` renders as `CAST(expr AS type)`. Because the arm cloned `kind`, a `::` input round-tripped to a `::` output. The pgvector and `::uuid` blob branches already returned before this code, so only the general scalar path was affected.

## Fix

The generic cast arm now forces `kind: CastKind::Cast` and stops binding the source `kind`. SQLite has no `::` operator and no `TRY_CAST` / `SAFE_CAST`, so `CastKind::Cast` is the only spelling it accepts, and collapsing `DoubleColon`, `TryCast`, and `SafeCast` to `Cast` is the correct best-effort translation. This matches every synthetic cast elsewhere in the file, which already builds `CastKind::Cast`.

## Tests

`tests/test_cast_operator.rs` covers the text, integer-literal, and numeric casts, a nested `(a::int)::text` cast, an apply test that executes the `CAST(...)` output against SQLite, and regressions proving the pgvector and `::uuid` blob branches still fire.

## Follow-ups

- **Array casts** (`expr::type[]`, carried on the `array` field or `DataType::Array`) still cannot be made valid in SQLite. They should return `Error::UnsupportedSQLiteFeature` rather than emit `CAST(x AS type ARRAY)`. This overlaps with the array-translation work that is blocked upstream.
- **The `format` field** (`CAST(x AS type FORMAT '...')`) is still cloned through. SQLite does not support cast formats, so this should be dropped or error.
- **Array literals** (`SELECT ARRAY[1, 2, 3]`, benchmark case AD3) are a sibling issue: the literal is passed through verbatim and fails at runtime instead of erroring at translation time. Tracked separately with the array work.
