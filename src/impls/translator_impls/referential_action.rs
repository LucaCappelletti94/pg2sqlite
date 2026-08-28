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

use sqlparser::ast::ReferentialAction;

crate::traits::translator::impl_contextual_translator!(
    ReferentialAction => ReferentialAction
);
impl crate::traits::translator::TranslatorWithContext for ReferentialAction {
    fn translate_with_warnings(
        &self,
        _schema: &Self::Schema,
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
