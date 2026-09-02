//! Semantic tests for pgvector to sqlite-vec translation.
//!
//! These tests verify that the translated SQL actually works with sqlite-vec,
//! triggers sync correctly, and KNN queries return correct results.
//!
//! # Performance Limitation
//!
//! sqlite-vec v0.1.x uses brute-force search (O(n)), not ANN indexing (O(log
//! n)). The vec0 virtual table provides optimized storage and SIMD
//! acceleration, yielding ~1.5x speedup over naive table scans, but not the
//! O(log n) performance of true indexed search (HNSW, IVFFlat, etc.).
//!
//! ANN support is actively being developed:
//! <https://github.com/asg017/sqlite-vec/issues/25>
//!
//! The translation is correct and will automatically benefit when ANN is added.

#![allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]

use std::time::Instant;

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use rand::RngExt;
use rusqlite::{Connection, Result, ffi::sqlite3_auto_extension};
use sqlite_vec::sqlite3_vec_init;
use zerocopy::IntoBytes;

/// Register sqlite-vec extension globally before any tests run.
fn register_sqlite_vec() {
    unsafe {
        sqlite3_auto_extension(Some(std::mem::transmute::<
            *const (),
            unsafe extern "C" fn(
                *mut rusqlite::ffi::sqlite3,
                *mut *mut i8,
                *const rusqlite::ffi::sqlite3_api_routines,
            ) -> i32,
        >(sqlite3_vec_init as *const ())));
    }
}

/// Generate a random f32 vector of the given dimension.
fn random_vector(dim: usize) -> Vec<f32> {
    let mut rng = rand::rng();
    (0..dim).map(|_| rng.random::<f32>()).collect()
}

#[test]
fn test_sqlite_vec_loaded() -> Result<()> {
    register_sqlite_vec();

    let db = Connection::open_in_memory()?;

    let version: String = db.query_row("SELECT vec_version()", [], |row| row.get(0))?;

    assert!(!version.is_empty(), "vec_version() should return a version string");
    println!("sqlite-vec version: {version}");

    Ok(())
}

#[test]
fn test_translated_vector_table_works() -> Result<()> {
    register_sqlite_vec();

    let sql = "
        CREATE TABLE items (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            embedding vector(4)
        );
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql).unwrap().translate(&options).unwrap();

    let db = Connection::open_in_memory()?;

    // Execute all translated statements
    for stmt in &translated {
        let sql_str = stmt.to_string();
        db.execute_batch(&sql_str)?;
    }

    // Insert some data - the triggers should sync to vec0
    let v1: Vec<f32> = vec![1.0, 0.0, 0.0, 0.0];
    let v2: Vec<f32> = vec![0.0, 1.0, 0.0, 0.0];
    let v3: Vec<f32> = vec![0.0, 0.0, 1.0, 0.0];

    db.execute("INSERT INTO items (id, name, embedding) VALUES (1, 'first', ?)", [v1.as_bytes()])?;
    db.execute("INSERT INTO items (id, name, embedding) VALUES (2, 'second', ?)", [v2.as_bytes()])?;
    db.execute("INSERT INTO items (id, name, embedding) VALUES (3, 'third', ?)", [v3.as_bytes()])?;

    // Verify data is in the main table
    let count: i32 = db.query_row("SELECT COUNT(*) FROM items", [], |row| row.get(0))?;
    assert_eq!(count, 3);

    // Verify data was synced to vec0 table via triggers
    let vec_count: i32 =
        db.query_row("SELECT COUNT(*) FROM items_embedding_vec", [], |row| row.get(0))?;
    assert_eq!(vec_count, 3, "Triggers should have synced data to vec0 table");

    Ok(())
}

#[test]
fn test_vec0_knn_query_correctness() -> Result<()> {
    register_sqlite_vec();

    let sql = "
        CREATE TABLE items (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            embedding vector(4)
        );
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql).unwrap().translate(&options).unwrap();

    let db = Connection::open_in_memory()?;

    for stmt in &translated {
        db.execute_batch(&stmt.to_string())?;
    }

    // Insert vectors at known positions
    let v1: Vec<f32> = vec![1.0, 0.0, 0.0, 0.0]; // Unit vector along x
    let v2: Vec<f32> = vec![0.0, 1.0, 0.0, 0.0]; // Unit vector along y
    let v3: Vec<f32> = vec![0.9, 0.1, 0.0, 0.0]; // Close to v1

    db.execute("INSERT INTO items (id, name, embedding) VALUES (1, 'x-axis', ?)", [v1.as_bytes()])?;
    db.execute("INSERT INTO items (id, name, embedding) VALUES (2, 'y-axis', ?)", [v2.as_bytes()])?;
    db.execute("INSERT INTO items (id, name, embedding) VALUES (3, 'near-x', ?)", [v3.as_bytes()])?;

    // Query for nearest neighbor to [1,0,0,0] - should be id=1 (exact match)
    // then id=3 (close)
    let query_vec: Vec<f32> = vec![1.0, 0.0, 0.0, 0.0];

    let mut stmt = db.prepare(
        "SELECT id_id, distance FROM items_embedding_vec \
         WHERE embedding MATCH ? \
         ORDER BY distance \
         LIMIT 3",
    )?;

    let results: Vec<(i32, f64)> = stmt
        .query_map([query_vec.as_bytes()], |row| Ok((row.get(0)?, row.get(1)?)))?
        .flatten()
        .collect();

    assert_eq!(results.len(), 3);
    // First result should be id=1 (exact match, distance ~0)
    assert_eq!(results[0].0, 1, "Nearest neighbor should be id=1");
    assert!(results[0].1 < 0.01, "Distance to exact match should be ~0");
    // Second should be id=3 (close to query)
    assert_eq!(results[1].0, 3, "Second nearest should be id=3");

    Ok(())
}

/// Test that vec0 indexed KNN works with large datasets and compare
/// performance.
///
/// This test inserts many vectors and verifies:
/// 1. vec0 MATCH query works correctly
/// 2. Both indexed and brute-force return valid results
/// 3. Reports performance comparison (informational)
#[test]
fn test_vec0_indexed_knn_large_dataset() -> Result<()> {
    register_sqlite_vec();

    const NUM_VECTORS: usize = 10_000;
    const DIMENSIONS: usize = 128;
    const K: usize = 10;

    let sql = "
        CREATE TABLE items (
            id INTEGER PRIMARY KEY,
            embedding vector(128)
        );
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql).unwrap().translate(&options).unwrap();

    let db = Connection::open_in_memory()?;

    for stmt in &translated {
        db.execute_batch(&stmt.to_string())?;
    }

    // Insert many random vectors
    println!("Inserting {NUM_VECTORS} vectors of dimension {DIMENSIONS}...");
    let insert_start = Instant::now();

    {
        let tx = db.unchecked_transaction()?;
        let mut insert_stmt = db.prepare("INSERT INTO items (id, embedding) VALUES (?, ?)")?;

        for i in 0..NUM_VECTORS {
            let v = random_vector(DIMENSIONS);
            insert_stmt.execute(rusqlite::params![i as i32, v.as_bytes()])?;
        }
        tx.commit()?;
    }

    println!("Insert took {:?}", insert_start.elapsed());

    // Generate a random query vector
    let query_vec = random_vector(DIMENSIONS);

    // Test vec0 indexed KNN
    let indexed_start = Instant::now();
    let mut indexed_stmt = db.prepare(
        "SELECT id_id, distance FROM items_embedding_vec \
         WHERE embedding MATCH ? \
         ORDER BY distance \
         LIMIT ?",
    )?;

    let indexed_results: Vec<(i32, f64)> = indexed_stmt
        .query_map(rusqlite::params![query_vec.as_bytes(), K as i32], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?
        .flatten()
        .collect();

    let indexed_time = indexed_start.elapsed();

    // Test brute-force (compute distance for all vectors)
    let brute_force_start = Instant::now();
    let mut brute_stmt = db.prepare(
        "SELECT id, vec_distance_L2(embedding, ?) as dist FROM items \
         ORDER BY dist \
         LIMIT ?",
    )?;

    let brute_results: Vec<(i32, f64)> = brute_stmt
        .query_map(rusqlite::params![query_vec.as_bytes(), K as i32], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?
        .flatten()
        .collect();

    let brute_force_time = brute_force_start.elapsed();

    println!("Performance comparison:");
    println!("  Indexed KNN (vec0):   {:?}", indexed_time);
    println!("  Brute-force:          {:?}", brute_force_time);
    println!(
        "  Speedup:              {:.2}x",
        brute_force_time.as_secs_f64() / indexed_time.as_secs_f64()
    );

    // Verify both methods return results
    assert_eq!(indexed_results.len(), K, "Indexed query should return K results");
    assert_eq!(brute_results.len(), K, "Brute-force query should return K results");

    // Verify results have valid distances (non-negative)
    assert!(
        indexed_results.iter().all(|(_, d)| *d >= 0.0),
        "All indexed distances should be non-negative"
    );
    assert!(
        brute_results.iter().all(|(_, d)| *d >= 0.0),
        "All brute-force distances should be non-negative"
    );

    // Verify results are sorted by distance
    for window in indexed_results.windows(2) {
        assert!(window[0].1 <= window[1].1, "Indexed results should be sorted by distance");
    }
    for window in brute_results.windows(2) {
        assert!(window[0].1 <= window[1].1, "Brute-force results should be sorted by distance");
    }

    println!("\nTop 5 indexed results: {:?}", &indexed_results[..5.min(indexed_results.len())]);
    println!("Top 5 brute-force results: {:?}", &brute_results[..5.min(brute_results.len())]);

    Ok(())
}

/// Test that vec0 provides meaningful performance improvement at scale.
///
/// This test uses 100k vectors to demonstrate that vec0's optimized storage
/// provides faster queries than naive brute-force on a regular BLOB column.
///
/// Note: sqlite-vec v0.1.x uses brute-force search (no ANN yet), but vec0's
/// chunked storage and SIMD-optimized distance calculations still provide
/// ~1.3-1.5x speedup over naive table scans. Future versions with IVF/HNSW
/// will provide much greater speedups.
///
/// See: https://github.com/asg017/sqlite-vec/issues/25
#[test]
fn test_vec0_faster_than_naive_at_scale() -> Result<()> {
    register_sqlite_vec();

    const NUM_VECTORS: usize = 100_000;
    const DIMENSIONS: usize = 64;
    const K: usize = 10;
    const NUM_QUERIES: usize = 10;

    let sql = "
        CREATE TABLE items (
            id INTEGER PRIMARY KEY,
            embedding vector(64)
        );
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql).unwrap().translate(&options).unwrap();

    let db = Connection::open_in_memory()?;

    for stmt in &translated {
        db.execute_batch(&stmt.to_string())?;
    }

    // Insert many random vectors
    println!("Inserting {NUM_VECTORS} vectors of dimension {DIMENSIONS}...");
    let insert_start = Instant::now();

    {
        let tx = db.unchecked_transaction()?;
        let mut insert_stmt = db.prepare("INSERT INTO items (id, embedding) VALUES (?, ?)")?;

        for i in 0..NUM_VECTORS {
            let v = random_vector(DIMENSIONS);
            insert_stmt.execute(rusqlite::params![i as i32, v.as_bytes()])?;
        }
        tx.commit()?;
    }

    println!("Insert took {:?}", insert_start.elapsed());

    // Run multiple queries and accumulate times
    let mut indexed_total = std::time::Duration::ZERO;
    let mut brute_total = std::time::Duration::ZERO;

    for _ in 0..NUM_QUERIES {
        let query_vec = random_vector(DIMENSIONS);

        // Benchmark vec0 KNN (optimized chunked storage + SIMD)
        let indexed_start = Instant::now();
        let mut indexed_stmt = db.prepare_cached(
            "SELECT id_id, distance FROM items_embedding_vec \
             WHERE embedding MATCH ? \
             ORDER BY distance \
             LIMIT ?",
        )?;

        let _indexed_results: Vec<(i32, f64)> = indexed_stmt
            .query_map(rusqlite::params![query_vec.as_bytes(), K as i32], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?
            .flatten()
            .collect();

        indexed_total += indexed_start.elapsed();

        // Benchmark naive brute-force on regular table
        let brute_start = Instant::now();
        let mut brute_stmt = db.prepare_cached(
            "SELECT id, vec_distance_L2(embedding, ?) as dist FROM items \
             ORDER BY dist \
             LIMIT ?",
        )?;

        let _brute_results: Vec<(i32, f64)> = brute_stmt
            .query_map(rusqlite::params![query_vec.as_bytes(), K as i32], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?
            .flatten()
            .collect();

        brute_total += brute_start.elapsed();
    }

    let indexed_avg = indexed_total / NUM_QUERIES as u32;
    let brute_avg = brute_total / NUM_QUERIES as u32;
    let speedup = brute_avg.as_secs_f64() / indexed_avg.as_secs_f64();

    println!("\nPerformance comparison ({NUM_VECTORS} vectors, {NUM_QUERIES} queries):");
    println!("  vec0 KNN:        {:?} avg", indexed_avg);
    println!("  Naive table:     {:?} avg", brute_avg);
    println!("  Speedup:         {:.2}x", speedup);

    // sqlite-vec v0.1.x uses brute-force but with optimized storage.
    // We expect at least 1.25x speedup from chunked storage + SIMD.
    assert!(
        speedup >= 1.25,
        "vec0 should be at least 1.25x faster than naive table scan. \
         Actual speedup was only {speedup:.2}x (vec0: {indexed_avg:?}, naive: {brute_avg:?})"
    );

    println!("SUCCESS: vec0 is {speedup:.2}x faster than naive table scan!");

    Ok(())
}

#[test]
fn test_update_trigger_syncs_to_vec0() -> Result<()> {
    register_sqlite_vec();

    let sql = "
        CREATE TABLE items (
            id INTEGER PRIMARY KEY,
            embedding vector(4)
        );
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql).unwrap().translate(&options).unwrap();

    let db = Connection::open_in_memory()?;

    for stmt in &translated {
        db.execute_batch(&stmt.to_string())?;
    }

    // Insert initial vector
    let v1: Vec<f32> = vec![1.0, 0.0, 0.0, 0.0];
    db.execute("INSERT INTO items (id, embedding) VALUES (1, ?)", [v1.as_bytes()])?;

    // Update the vector
    let v2: Vec<f32> = vec![0.0, 1.0, 0.0, 0.0];
    db.execute("UPDATE items SET embedding = ? WHERE id = 1", [v2.as_bytes()])?;

    // Query vec0 - should find the updated vector
    let query_vec: Vec<f32> = vec![0.0, 1.0, 0.0, 0.0];
    let (id, distance): (i32, f64) = db.query_row(
        "SELECT id_id, distance FROM items_embedding_vec WHERE embedding MATCH ? LIMIT 1",
        [query_vec.as_bytes()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;

    assert_eq!(id, 1);
    assert!(distance < 0.01, "Should find the updated vector with ~0 distance");

    Ok(())
}

/// R126 update path: vec0 cannot store NULL, so an update TO NULL must not
/// forward NULL into the shadow table. It deletes the shadow row instead,
/// mirroring the insert trigger's rule that a NULL embedding stays out of
/// the index.
#[test]
fn an_update_to_null_removes_the_row_from_the_index() -> Result<()> {
    register_sqlite_vec();

    let sql = "
        CREATE TABLE items (
            id INTEGER PRIMARY KEY,
            embedding vector(4)
        );
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql).unwrap().translate(&options).unwrap();

    let db = Connection::open_in_memory()?;
    for stmt in &translated {
        db.execute_batch(&stmt.to_string())?;
    }

    let v1: Vec<f32> = vec![1.0, 0.0, 0.0, 0.0];
    db.execute("INSERT INTO items (id, embedding) VALUES (1, ?)", [v1.as_bytes()])?;
    let count: i32 =
        db.query_row("SELECT COUNT(*) FROM items_embedding_vec", [], |row| row.get(0))?;
    assert_eq!(count, 1, "the inserted vector should be indexed");

    db.execute("UPDATE items SET embedding = NULL WHERE id = 1", [])
        .expect("an update to NULL is valid PostgreSQL and must not crash on the sync trigger");

    let count: i32 =
        db.query_row("SELECT COUNT(*) FROM items_embedding_vec", [], |row| row.get(0))?;
    assert_eq!(count, 0, "a NULL embedding cannot stay in the vec0 index");

    Ok(())
}

/// R126 update path: a row born with a NULL embedding has no shadow row, so
/// the old in-place UPDATE matched nothing and the row silently never joined
/// the index. The trigger must create the shadow row.
#[test]
fn a_null_row_updated_to_a_vector_joins_the_index() -> Result<()> {
    register_sqlite_vec();

    let sql = "
        CREATE TABLE items (
            id INTEGER PRIMARY KEY,
            embedding vector(4)
        );
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql).unwrap().translate(&options).unwrap();

    let db = Connection::open_in_memory()?;
    for stmt in &translated {
        db.execute_batch(&stmt.to_string())?;
    }

    db.execute("INSERT INTO items (id, embedding) VALUES (1, NULL)", [])?;
    let count: i32 =
        db.query_row("SELECT COUNT(*) FROM items_embedding_vec", [], |row| row.get(0))?;
    assert_eq!(count, 0, "a NULL embedding stays out of the index at insert");

    let v1: Vec<f32> = vec![0.0, 1.0, 0.0, 0.0];
    db.execute("UPDATE items SET embedding = ? WHERE id = 1", [v1.as_bytes()])?;

    let (id, distance): (i32, f64) = db.query_row(
        "SELECT id_id, distance FROM items_embedding_vec WHERE embedding MATCH ? LIMIT 1",
        [v1.as_bytes()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(id, 1, "the updated row must join the index");
    assert!(distance < 0.01, "the indexed value must be the updated vector");

    Ok(())
}

/// Guard for the trigger's other duty: a primary key update moves the shadow
/// row to the new key.
#[test]
fn a_pk_update_moves_the_indexed_row() -> Result<()> {
    register_sqlite_vec();

    let sql = "
        CREATE TABLE items (
            id INTEGER PRIMARY KEY,
            embedding vector(4)
        );
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql).unwrap().translate(&options).unwrap();

    let db = Connection::open_in_memory()?;
    for stmt in &translated {
        db.execute_batch(&stmt.to_string())?;
    }

    let v1: Vec<f32> = vec![1.0, 0.0, 0.0, 0.0];
    db.execute("INSERT INTO items (id, embedding) VALUES (1, ?)", [v1.as_bytes()])?;
    db.execute("UPDATE items SET id = 2 WHERE id = 1", [])?;

    let indexed_pk: i32 =
        db.query_row("SELECT id_id FROM items_embedding_vec", [], |row| row.get(0))?;
    assert_eq!(indexed_pk, 2, "the shadow row must follow the primary key");

    Ok(())
}

#[test]
fn test_delete_trigger_removes_from_vec0() -> Result<()> {
    register_sqlite_vec();

    let sql = "
        CREATE TABLE items (
            id INTEGER PRIMARY KEY,
            embedding vector(4)
        );
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql).unwrap().translate(&options).unwrap();

    let db = Connection::open_in_memory()?;

    for stmt in &translated {
        db.execute_batch(&stmt.to_string())?;
    }

    // Insert vectors
    let v1: Vec<f32> = vec![1.0, 0.0, 0.0, 0.0];
    let v2: Vec<f32> = vec![0.0, 1.0, 0.0, 0.0];
    db.execute("INSERT INTO items (id, embedding) VALUES (1, ?)", [v1.as_bytes()])?;
    db.execute("INSERT INTO items (id, embedding) VALUES (2, ?)", [v2.as_bytes()])?;

    // Verify both are in vec0
    let count: i32 =
        db.query_row("SELECT COUNT(*) FROM items_embedding_vec", [], |row| row.get(0))?;
    assert_eq!(count, 2);

    // Delete one
    db.execute("DELETE FROM items WHERE id = 1", [])?;

    // Verify only one remains in vec0
    let count: i32 =
        db.query_row("SELECT COUNT(*) FROM items_embedding_vec", [], |row| row.get(0))?;
    assert_eq!(count, 1);

    // The remaining one should be id=2
    let remaining_id: i32 =
        db.query_row("SELECT id_id FROM items_embedding_vec", [], |row| row.get(0))?;
    assert_eq!(remaining_id, 2);

    Ok(())
}
