//! Implementation of the [`Translator`] trait for the
//! `ReferentialAction` type.

#[cfg(not(feature = "std"))]
#[allow(unused_imports)]
use alloc::{
    borrow::ToOwned,
    boxed::Box,
    format,
    string::{String, ToString},
    vec,
    vec::Vec,
};

use sql_traits::structs::ParserDB;
use sqlparser::ast::ReferentialAction;

use crate::prelude::{Pg2SqliteOptions, Translator};

impl Translator for ReferentialAction {
    type Schema = ParserDB;
    type Options = Pg2SqliteOptions;
    type SQLiteEntry = ReferentialAction;

    fn translate(
        &self,
        _schema: &Self::Schema,
        _options: &Self::Options,
    ) -> Result<Self::SQLiteEntry, crate::errors::Error> {
        match self {
            ReferentialAction::NoAction => Ok(ReferentialAction::NoAction),
            ReferentialAction::Restrict => Ok(ReferentialAction::Restrict),
            ReferentialAction::SetNull => Ok(ReferentialAction::SetNull),
            ReferentialAction::SetDefault => Ok(ReferentialAction::SetDefault),
            ReferentialAction::Cascade => Ok(ReferentialAction::Cascade),
        }
    }
}
