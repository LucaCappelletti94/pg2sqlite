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
use sqlparser::{
    ast::{DataType, ExactNumberInfo, Expr, Ident, Value, ValueWithSpan},
    tokenizer::Span,
};

use crate::{
    prelude::{Pg2SqliteOptions, Translator},
    traits::{TranslationOptions, UuidRepresentation},
};

/// The largest precision a scaled integer can hold.
///
/// The biggest minor-unit value for `NUMERIC(p,s)` is `10^p - 1`, and i64 stops
/// at 9223372036854775807, so 18 digits fit and 19 do not. `NUMERIC(18,2)`
/// still reaches about 10^16, well past any money.
pub(crate) const MAX_NUMERIC_PRECISION: u64 = 18;

/// [`MAX_NUMERIC_PRECISION`] as an exponent, for the one conversion that needs
/// a fallback the debug assertion above it rules out.
pub(crate) const MAX_NUMERIC_PRECISION_EXPONENT: u32 = 18;

/// The precision and scale of a `NUMERIC` or `DECIMAL` declaration, refusing
/// the two shapes that cannot be a scaled integer.
///
/// Bare `NUMERIC` has arbitrary unconstrained scale, so there is no `10^s` to
/// scale by, and picking one would corrupt data. `NUMERIC(p)` is scale 0 in
/// PostgreSQL, which is already a plain integer.
pub(crate) fn numeric_precision_and_scale(
    info: &ExactNumberInfo,
) -> Result<(u64, u32), crate::errors::Error> {
    let (precision, scale) = match info {
        ExactNumberInfo::None => {
            return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                "NUMERIC and DECIMAL without a precision and scale have no SQLite form: the \
                 column is emitted as an INTEGER holding minor units, which needs a fixed scale \
                 to multiply by. Declare one, as NUMERIC(10,2)."
                    .to_string(),
            ));
        }
        ExactNumberInfo::Precision(precision) => (*precision, 0),
        ExactNumberInfo::PrecisionAndScale(precision, scale) => {
            let Ok(scale) = u32::try_from(*scale) else {
                return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
                    "NUMERIC({precision},{scale}) has a negative scale, which cannot be a count \
                     of minor units."
                )));
            };
            (*precision, scale)
        }
    };

    if precision > MAX_NUMERIC_PRECISION {
        return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
            "NUMERIC({precision},{scale}) needs {precision} digits, and a SQLite INTEGER holds \
             at most {MAX_NUMERIC_PRECISION}. The column would silently become a float, which is \
             what the scaled integer exists to avoid. Reduce the precision, or store the value \
             as TEXT and compare it in the application."
        )));
    }
    if u64::from(scale) > precision {
        return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
            "NUMERIC({precision},{scale}) has a scale larger than its precision."
        )));
    }
    Ok((precision, scale))
}

/// `<col> BETWEEN -(10^p - 1) AND (10^p - 1)`, with the bound expanded to a
/// literal so SQLite has no arithmetic to do per row.
///
/// `precision` has already passed [`numeric_precision_and_scale`], so it is at
/// most [`MAX_NUMERIC_PRECISION`] and the exponent conversion cannot lose
/// anything.
#[must_use]
pub(crate) fn numeric_precision_bound_expr(column_name: &Ident, precision: u64) -> Expr {
    debug_assert!(
        precision <= MAX_NUMERIC_PRECISION,
        "the bound is only meaningful for a precision a scaled integer can hold"
    );
    let exponent = u32::try_from(precision).unwrap_or(MAX_NUMERIC_PRECISION_EXPONENT);
    let magnitude = 10_i128.pow(exponent) - 1;
    let literal = |value: i128| {
        Expr::Value(ValueWithSpan {
            value: Value::Number(value.to_string(), false),
            span: Span::empty(),
        })
    };
    Expr::Between {
        expr: Box::new(Expr::Identifier(column_name.clone())),
        negated: false,
        low: Box::new(literal(-magnitude)),
        high: Box::new(literal(magnitude)),
    }
}

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
            | DataType::Float4 => Ok(DataType::Real),
            // NUMERIC and DECIMAL become an INTEGER holding minor units, scaled
            // by 10^s, which is the only representation SQLite has that keeps
            // decimal arithmetic exact. REAL does not: measured, `sum` over
            // 0.10 and 0.20 answers 0.30000000000000004 and `0.1 + 0.2 = 0.3`
            // is FALSE. See decision D1.
            DataType::Numeric(info) | DataType::Decimal(info) => {
                numeric_precision_and_scale(info)?;
                Ok(DataType::Integer(None))
            }
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
                        // EWKB-encoded values produced by the `SQLiteGIS` SQLite extension
                        // (https://github.com/LucaCappelletti94/sqlitegis) round-trip
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
