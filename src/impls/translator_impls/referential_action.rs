//! Implementation of the [`Translator`](crate::traits::Translator) trait for
//! the `ReferentialAction` type.

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

use sqlparser::ast::ReferentialAction;

impl crate::traits::translator::TranslatorWithContext for ReferentialAction {
    type SQLiteEntry = ReferentialAction;

    fn translate_with_warnings(
        &self,
        _schema: &sql_traits::structs::ParserDB,
        _options: &crate::options::TranslationContext<'_>,
        _emit: &mut dyn FnMut(crate::warnings::TranslationWarning),
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
