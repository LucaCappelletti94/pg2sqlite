//! Implementation of the [`Translator`] trait for the
//! `Function` type.

use sql_traits::structs::ParserDB;
use sqlparser::ast::{Function, Ident, ObjectName};

use crate::prelude::{Pg2SqliteOptions, Translator};

fn translate_function_name(name: &ObjectName) -> Result<ObjectName, crate::errors::Error> {
    let original_name = name.to_string();
    let translated_name = match original_name.to_lowercase().as_str() {
        "least" => "MIN",
        "greatest" => "MAX",
        "ts_rank" | "ts_rank_cd" => {
            return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                "ts_rank/ts_rank_cd are not directly translatable to SQLite. \
                 FTS5 provides bm25() for ranking, but it requires a different query structure. \
                 Consider querying the FTS5 table directly: \
                 SELECT *, bm25(table_fts) AS rank FROM table_fts WHERE table_fts MATCH 'query' ORDER BY rank"
                    .to_string(),
            ));
        }
        _ => return Ok(name.clone()),
    };
    Ok(ObjectName::from(vec![Ident::new(translated_name.to_string())]))
}

impl Translator for Function {
    type Schema = ParserDB;
    type Options = Pg2SqliteOptions;
    type SQLiteEntry = Self;

    fn translate(
        &self,
        _schema: &Self::Schema,
        _options: &Self::Options,
    ) -> Result<Self::SQLiteEntry, crate::errors::Error> {
        Ok(Function {
            name: translate_function_name(&self.name)?,
            uses_odbc_syntax: self.uses_odbc_syntax,
            parameters: self.parameters.clone(),
            args: self.args.clone(),
            filter: self.filter.clone(),
            null_treatment: self.null_treatment,
            over: self.over.clone(),
            within_group: self.within_group.clone(),
        })
    }
}
