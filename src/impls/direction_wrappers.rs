//! Macro helpers for defining direction-specific helper wrappers.

macro_rules! define_direction_wrappers {
    (
        direction = $direction:ty;
        $(
            fn $fn_name:ident($arg_name:ident : $arg_ty:ty) -> $ret_ty:ty = $shared_fn:ident;
        )+
    ) => {
        $(
            pub(super) fn $fn_name(
                $arg_name: $arg_ty,
                schema: &sql_traits::structs::ParserDB,
                options: &crate::prelude::Pg2SqliteOptions,
            ) -> Result<$ret_ty, crate::errors::Error> {
                crate::impls::shared_helpers::$shared_fn::<$direction>($arg_name, schema, options)
            }
        )+
    };
}

pub(crate) use define_direction_wrappers;
