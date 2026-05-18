//! Implementation of the [`Translator`] trait for the
//! `DataType` type.

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
            // Float aliases
            | DataType::Double(_)
            | DataType::DoublePrecision
            | DataType::Float8
            | DataType::Float4
            // Numeric/Decimal: mapping is intentionally lossy — SQLite has no fixed-precision type
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
            // All TIMESTAMP variants (with/without TZ, with/without precision).
            | DataType::Timestamp(_, _)
            // Other temporal types.
            | DataType::Date
            | DataType::Datetime(_)
            | DataType::Time(_, _)
            | DataType::Interval { .. } => Ok(DataType::Text),
            // Bit types map to INTEGER (SQLite has no native bit type)
            DataType::Bit(_) | DataType::BitVarying(_) | DataType::VarBit(_) => {
                Ok(DataType::Integer(None))
            }
            // Arrays are not yet supported - they could map to JSON or vector extensions
            // depending on use case (regular arrays vs embeddings)
            DataType::Array(inner) => {
                Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
                    "Array type {inner:?} is not supported. Arrays could be JSON-serialized or \
                     use vector extensions depending on use case.",
                )))
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
                        // extension loaded; see `Pg2SqliteOptions::with_geolite_enabled`
                        // for runtime `ST_*` function passthrough.
                        Ok(DataType::Blob(None))
                    }
                    Some("countrycode") => {
                        // SQLite does not have a country code type, so we use TEXT instead.
                        Ok(DataType::Text)
                    }
                    Some("cas" | "molecularformula" | "mediatype") => {
                        // SQLite does not have a CAS type, so we use BLOB instead.
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

#[cfg(test)]
mod tests {
    use sql_traits::structs::ParserDB;
    use sqlparser::ast::DataType;

    use crate::prelude::{Pg2SqliteOptions, Translator};

    fn empty_schema() -> ParserDB {
        ParserDB::from_statements(Vec::new(), "test".to_string()).expect("schema should build")
    }

    #[test]
    fn translate_reports_unsupported_for_unhandled_data_type_variants() {
        use sqlparser::ast::ArrayElemTypeDef;

        let schema = empty_schema();
        let options = Pg2SqliteOptions::default();

        // Array types are still unsupported — use one as a representative unsupported
        // variant
        let err = DataType::Array(ArrayElemTypeDef::None)
            .translate(&schema, &options)
            .expect_err("Array type should be rejected");
        assert!(err.to_string().contains("Array type"));
    }
}
