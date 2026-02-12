//! Fuzz target for SQL translation.
//!
//! This target feeds arbitrary byte sequences through the full parsing and
//! translation pipeline to find crashes or panics.

use honggfuzz::fuzz;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

fn main() {
    loop {
        fuzz!(|data: &[u8]| {
            // Skip very long inputs to avoid excessive processing time
            if data.len() > 500 {
                return;
            }

            // Convert bytes to string, skipping invalid UTF-8
            let Ok(sql) = std::str::from_utf8(data) else {
                return;
            };

            // Try to parse and translate - we don't care about errors, only panics
            if let Ok(parsed) = Pg2Sqlite::default().sql(sql) {
                let options = Pg2SqliteOptions::default();
                let _ = parsed.translate(&options);
            }
        });
    }
}
