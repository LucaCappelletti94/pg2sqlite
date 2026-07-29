//! Implementation of the [`Translator`] trait for the
//! `DataType` type.

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
use sqlparser::ast::{DataType, ExactNumberInfo};

use crate::{
    prelude::{Pg2SqliteOptions, Translator},
    traits::{TranslationOptions, UuidRepresentation},
};

impl Translator for DataType {
    type Schema = ParserDB;
    type Options = Pg2SqliteOptions;
    type SQLiteEntry = DataType;

    fn translate(
        &self,
        _schema: &Self::Schema,
        options: &Self::Options,
    ) -> Result<Self::SQLiteEntry, crate::errors::Error> {
        match self {
            DataType::Text | DataType::Integer(None) | DataType::Real | DataType::Blob(None) => {
                Ok(self.clone())
            }
            // BLOB with a precision (e.g. BLOB(50)) → drop the precision, map to plain BLOB.
            // Bytea and binary aliases use the same storage class.
            DataType::Blob(Some(_))
            | DataType::Bytea
            | DataType::Binary(_)
            | DataType::Varbinary(_) => Ok(DataType::Blob(None)),
            // Integer types with optional display-width precision (e.g. INT(11) in MySQL).
            // The precision is a display hint only; drop it and map to SQLite INTEGER.
            DataType::SmallInt(_)
            | DataType::Int(_)
            | DataType::Integer(Some(_))
            | DataType::Boolean
            | DataType::Bool
            | DataType::BigInt(_)
            | DataType::Int8(_)
            | DataType::Int4(_)
            | DataType::Int2(_)
            | DataType::TinyInt(_)
            // INT64 is a BigQuery-specific alias for 64-bit integer
            | DataType::Int64 => Ok(DataType::Integer(None)),
            DataType::Float(ExactNumberInfo::None)
            | DataType::Double(_)
            | DataType::DoublePrecision
            | DataType::Float8
            | DataType::Float4
            // Numeric/Decimal: mapping is intentionally lossy - SQLite has no fixed-precision type
            | DataType::Numeric(_)
            | DataType::Decimal(_) => Ok(DataType::Real),
            // JSON/JSONB, text aliases, and temporal types are stored as TEXT in SQLite.
            DataType::Varchar(_)
            | DataType::JSON
            | DataType::JSONB
            | DataType::Clob(_)
            | DataType::Nvarchar(_)
            | DataType::Enum(_, _)
            | DataType::TsVector
            | DataType::TsQuery
            | DataType::Timestamp(_, _)
            | DataType::Date
            | DataType::Datetime(_)
            | DataType::Time(_, _)
            | DataType::Interval { .. } => Ok(DataType::Text),
            // Bit types map to INTEGER (SQLite has no native bit type)
            DataType::Bit(_) | DataType::BitVarying(_) | DataType::VarBit(_) => {
                Ok(DataType::Integer(None))
            }
            // PostgreSQL arrays become JSON array text under
            // `ArrayRepresentation::Json`; `super::array` carries the matching
            // expression-level rewrites. pgvector embeddings are a separate
            // path: they arrive as `DataType::Custom("vector")`, not as an
            // array type, and map to BLOB below.
            DataType::Array(_) => {
                if super::array::is_json_array_representation(options) {
                    Ok(DataType::Text)
                } else {
                    Err(super::array::representation_required(&format!(
                        "The array type {self}"
                    )))
                }
            }
            DataType::Uuid => {
                match options.get_uuid_representation() {
                    Some(UuidRepresentation::Blob) => Ok(DataType::Blob(None)),
                    Some(UuidRepresentation::Text) => Ok(DataType::Text),
                    None => {
                        Err(crate::errors::Error::UnsupportedSQLiteFeature(
                            "UUID translation requires specifying a representation (TEXT or BLOB)"
                                .to_string(),
                        ))
                    }
                }
            }
            DataType::Custom(name, ..) => {
                let custom_type_name = name
                    .0
                    .last()
                    .and_then(|part| part.as_ident())
                    .map(|ident| ident.value.to_ascii_lowercase());

                match custom_type_name.as_deref() {
                    Some("serial" | "smallserial" | "bigserial" | "largeserial") => {
                        Ok(DataType::Integer(None))
                    }
                    Some("geometry" | "geography") => {
                        // PostGIS `geometry` and `geography` translate to BLOB so that
                        // EWKB-encoded values produced by the `geolite` SQLite extension
                        // (https://github.com/LucaCappelletti94/geolite) round-trip
                        // through the column. The blob is opaque to SQLite without the
                        // extension loaded; see `Pg2SqliteOptions::with_sqlitegis_enabled`
                        // for runtime `ST_*` function passthrough.
                        Ok(DataType::Blob(None))
                    }
                    Some("countrycode") => {
                        Ok(DataType::Text)
                    }
                    Some("cas" | "molecularformula" | "mediatype") => {
                        Ok(DataType::Blob(None))
                    }
                    // pgvector types: vector(N) and halfvec(N) -> BLOB for sqlite-vec
                    // The vector data is stored as BLOB in the main table, with a companion
                    // vec0 virtual table for indexed KNN search.
                    Some("vector" | "halfvec") => Ok(DataType::Blob(None)),
                    unknown => {
                        Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
                            "Unknown PostgreSQL custom type {unknown:?}"
                        )))
                    }
                }
            }
            unsupported => {
                Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
                    "The data type {unsupported:?} is not supported"
                )))
            }
        }
    }
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use sql_traits::structs::ParserDB;
    use sqlparser::ast::DataType;

    use crate::{
        prelude::{Pg2SqliteOptions, Translator},
        traits::TranslationOptions,
    };

    fn empty_schema() -> ParserDB {
        ParserDB::from_statements(Vec::new(), "test".to_string()).expect("schema should build")
    }

    #[test]
    fn array_type_needs_a_representation() {
        use sqlparser::ast::ArrayElemTypeDef;

        let schema = empty_schema();
        let err = DataType::Array(ArrayElemTypeDef::None)
            .translate(&schema, &Pg2SqliteOptions::default())
            .expect_err("array type should be rejected without a representation");
        assert!(
            err.to_string().contains("with_array_representation"),
            "error should name the opt-in, got: {err}"
        );
    }

    #[test]
    fn array_type_maps_to_text_under_json_representation() {
        use sqlparser::ast::{ArrayElemTypeDef, DataType as Dt};

        use crate::traits::ArrayRepresentation;

        let schema = empty_schema();
        let options =
            Pg2SqliteOptions::default().with_array_representation(ArrayRepresentation::Json);
        for elem in [
            ArrayElemTypeDef::None,
            ArrayElemTypeDef::SquareBracket(Box::new(Dt::Int(None)), None),
            ArrayElemTypeDef::Qualified(Box::new(Dt::Int(None)), Some(4)),
        ] {
            assert_eq!(
                DataType::Array(elem).translate(&schema, &options).expect("array should translate"),
                DataType::Text
            );
        }
    }
}
