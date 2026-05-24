//! Fuzz target for SQL translation.
//!
//! Feeds arbitrary byte sequences through the full parsing and
//! translation pipeline to find crashes or panics.

#![no_main]

use libfuzzer_sys::fuzz_target;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

fuzz_target!(|data: &[u8]| {
    if data.len() > 500 {
        return;
    }
    let Ok(sql) = std::str::from_utf8(data) else {
        return;
    };
    if let Ok(parsed) = Pg2Sqlite::default().sql(sql) {
        let options = Pg2SqliteOptions::default();
        let _ = parsed.translate(&options);
    }
});
