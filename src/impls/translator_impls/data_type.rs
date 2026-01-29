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
            DataType::Text | DataType::Integer(None) | DataType::Real => Ok(self.clone()),
            DataType::SmallInt(None) | DataType::Int(None) | DataType::Boolean | DataType::Bool => {
                Ok(DataType::Integer(None))
            }
            DataType::Float(ExactNumberInfo::None) => Ok(DataType::Real),
            DataType::Bytea => Ok(DataType::Blob(None)),
            DataType::Varchar(_) => Ok(DataType::Text),
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
                    unimplemented => {
                        unimplemented!("The data type {:?} is not supported", unimplemented)
                    }
                }
            }
            unimplemented => unimplemented!("The data type {:?} is not supported", unimplemented),
        }
    }
}
