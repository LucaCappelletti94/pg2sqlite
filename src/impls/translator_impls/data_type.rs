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
            DataType::SmallInt(None) | DataType::Int(None) | DataType::Boolean | DataType::Bool => {
                Ok(DataType::Integer(None))
            }
            DataType::Float(ExactNumberInfo::None) => Ok(DataType::Real),
            DataType::Bytea => Ok(DataType::Blob(None)),
            // JSON/JSONB are stored as TEXT in SQLite
            DataType::Varchar(_) | DataType::JSON | DataType::JSONB => Ok(DataType::Text),
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
            DataType::Timestamp(None, sqlparser::ast::TimezoneInfo::WithTimeZone) => {
                // SQLite does not support timezone information, and these type of
                // fields are commonly converted to TEXT.
                Ok(DataType::Text)
            }
            DataType::Timestamp(None, sqlparser::ast::TimezoneInfo::None) => {
                // SQLite does not support actually support timestamp, but emulates it
                // with several different types. Since in the `diesel` library the backend
                // type is `Text`, we will use that as well.
                Ok(DataType::Text)
            }
            DataType::Custom(name, ..) => {
                match name.0.first().and_then(|s| Some(s.as_ident()?.value.as_str())) {
                    Some("SERIAL" | "SMALLSERIAL") => Ok(DataType::Integer(None)),
                    Some("GEOGRAPHY") => {
                        // SQLite does not have postgis support, but we have implemented
                        // support in the `postgis-diesel` crate for the `geometry` and
                        // `geography` types, both of which use `BLOB` in SQLite.
                        Ok(DataType::Blob(None))
                    }
                    Some("countrycode" | "CountryCode") => {
                        // SQLite does not have a country code type, so we use TEXT instead.
                        Ok(DataType::Text)
                    }
                    Some("cas" | "CAS" | "MolecularFormula" | "molecularformula" | "MediaType") => {
                        // SQLite does not have a CAS type, so we use BLOB instead.
                        Ok(DataType::Binary(None))
                    }
                    // pgvector types: vector(N) and halfvec(N) -> BLOB for sqlite-vec
                    // The vector data is stored as BLOB in the main table, with a companion
                    // vec0 virtual table for indexed KNN search.
                    Some("vector" | "VECTOR" | "halfvec" | "HALFVEC") => Ok(DataType::Blob(None)),
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
        let schema = empty_schema();
        let options = Pg2SqliteOptions::default();

        let err = DataType::Date.translate(&schema, &options).expect_err("DATE should be rejected");
        assert!(err.to_string().contains("is not supported"));
    }
}
