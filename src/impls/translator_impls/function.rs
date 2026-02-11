//! Implementation of the [`Translator`] trait for the
//! `Function` type.

use sql_traits::structs::ParserDB;
use sqlparser::ast::{
    Expr, Function, FunctionArg, FunctionArgExpr, FunctionArgumentList, FunctionArguments, Ident,
    ObjectName, Value, ValueWithSpan,
};

use crate::prelude::{Pg2SqliteOptions, Translator};

/// Represents a function translation result.
enum FunctionTranslation {
    /// Simple name replacement (e.g., LEAST -> MIN)
    Rename(String),
    /// Function with modified arguments (e.g., NOW() -> datetime('now'))
    WithArgs { name: String, args: Vec<FunctionArg> },
    /// Unsupported function with error message
    Unsupported(String),
    /// No translation needed
    PassThrough,
}

fn translate_function(name: &ObjectName, _args: &FunctionArguments) -> FunctionTranslation {
    let original_name = name.to_string().to_lowercase();

    match original_name.as_str() {
        "least" => FunctionTranslation::Rename("MIN".to_string()),
        "greatest" => FunctionTranslation::Rename("MAX".to_string()),
        "now" => {
            // NOW() -> datetime('now')
            FunctionTranslation::WithArgs {
                name: "datetime".to_string(),
                args: vec![FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(
                    ValueWithSpan {
                        value: Value::SingleQuotedString("now".to_string()),
                        span: sqlparser::tokenizer::Span::empty(),
                    },
                )))],
            }
        }
        // string_agg -> group_concat (for SQLite < 3.44 compatibility)
        "string_agg" => FunctionTranslation::Rename("group_concat".to_string()),
        "ts_rank" | "ts_rank_cd" => FunctionTranslation::Unsupported(
            "ts_rank/ts_rank_cd are not directly translatable to SQLite. \
             FTS5 provides bm25() for ranking, but it requires a different query structure. \
             Consider querying the FTS5 table directly: \
             SELECT *, bm25(table_fts) AS rank FROM table_fts WHERE table_fts MATCH 'query' ORDER BY rank"
                .to_string(),
        ),
        _ => FunctionTranslation::PassThrough,
    }
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
        match translate_function(&self.name, &self.args) {
            FunctionTranslation::Rename(new_name) => {
                Ok(Function {
                    name: ObjectName::from(vec![Ident::new(new_name)]),
                    uses_odbc_syntax: self.uses_odbc_syntax,
                    parameters: self.parameters.clone(),
                    args: self.args.clone(),
                    filter: self.filter.clone(),
                    null_treatment: self.null_treatment,
                    over: self.over.clone(),
                    within_group: self.within_group.clone(),
                })
            }
            FunctionTranslation::WithArgs { name, args } => {
                Ok(Function {
                    name: ObjectName::from(vec![Ident::new(name)]),
                    uses_odbc_syntax: false,
                    parameters: FunctionArguments::None,
                    args: FunctionArguments::List(FunctionArgumentList {
                        duplicate_treatment: None,
                        args,
                        clauses: vec![],
                    }),
                    filter: None,
                    null_treatment: None,
                    over: None,
                    within_group: vec![],
                })
            }
            FunctionTranslation::Unsupported(msg) => {
                Err(crate::errors::Error::UnsupportedSQLiteFeature(msg))
            }
            FunctionTranslation::PassThrough => Ok(self.clone()),
        }
    }
}
