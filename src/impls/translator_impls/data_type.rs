//! Implementation of the [`Translator`](crate::traits::Translator) trait for
//! the `DataType` type.

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

use sqlparser::{
    ast::{
        BinaryOperator, CharLengthUnits, CharacterLength, DataType, ExactNumberInfo, Expr, Ident,
        Value, ValueWithSpan,
    },
    tokenizer::Span,
};

use crate::{impls::function_helpers::simple_function_expr, traits::UuidRepresentation};

/// The largest precision a scaled integer can hold.
///
/// The biggest minor-unit value for `NUMERIC(p,s)` is `10^p - 1`, and i64 stops
/// at 9223372036854775807, so 18 digits fit and 19 do not. `NUMERIC(18,2)`
/// still reaches about 10^16, well past any money.
pub(crate) const MAX_NUMERIC_PRECISION: u64 = 18;

/// [`MAX_NUMERIC_PRECISION`] as an exponent, for the one conversion that needs
/// a fallback the debug assertion above it rules out.
pub(crate) const MAX_NUMERIC_PRECISION_EXPONENT: u32 = 18;

/// The declared length of a character type, or `None` when it carries none.
///
/// `VARCHAR(MAX)` is T-SQL and bounds nothing. A length in octets is refused:
/// SQLite's `length()` counts characters, so it would enforce the wrong thing,
/// and PostgreSQL answers `syntax error at or near "OCTETS"` anyway.
pub(crate) fn character_length(data_type: &DataType) -> Result<Option<u64>, crate::errors::Error> {
    let (DataType::Char(declared)
    | DataType::Character(declared)
    | DataType::Varchar(declared)
    | DataType::CharacterVarying(declared)) = data_type
    else {
        return Ok(None);
    };

    match declared {
        None | Some(CharacterLength::Max) => Ok(None),
        Some(CharacterLength::IntegerLength { length, unit }) => {
            match unit {
                None | Some(CharLengthUnits::Characters) => Ok(Some(*length)),
                Some(other) => {
                    Err(crate::errors::Error::forward_refusal(format!(
                        "{data_type} declares its length in {other}, and SQLite's length() counts \
                         characters, so the bound would measure the wrong thing. Declare the \
                         length in characters."
                    )))
                }
            }
        }
    }
}

/// `length(<col>) <= n`, the bound PostgreSQL enforces on a declared character
/// length, refusing a longer value rather than truncating it.
#[must_use]
pub(crate) fn character_length_bound_expr(column_name: &Ident, length: u64) -> Expr {
    Expr::BinaryOp {
        left: Box::new(simple_function_expr(
            "length",
            vec![Expr::Identifier(column_name.clone())],
            None,
        )),
        op: BinaryOperator::LtEq,
        right: Box::new(Expr::Value(ValueWithSpan {
            value: Value::Number(length.to_string(), false),
            span: Span::empty(),
        })),
    }
}

/// The precision carrier shared by `NUMERIC`, `DECIMAL`, and `DEC`, one type
/// under three standard spellings in PostgreSQL.
///
/// Every expression-position site that treats a declared type as a scaled
/// integer resolves the spelling through this, so an alias added upstream is
/// added once here. The dispatch arm in the `DataType` translation below
/// spells the same three variants in its pattern and must stay in step.
pub(crate) fn exact_numeric_info(data_type: &DataType) -> Option<&ExactNumberInfo> {
    match data_type {
        DataType::Numeric(info) | DataType::Decimal(info) | DataType::Dec(info) => Some(info),
        _ => None,
    }
}

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
            return Err(crate::errors::Error::forward_refusal(
                "NUMERIC and DECIMAL without a precision and scale have no SQLite form: the \
             column is emitted as an INTEGER holding minor units, which needs a fixed scale \
             to multiply by. Declare one, as NUMERIC(10,2)."
                    .to_string(),
            ));
        }
        ExactNumberInfo::Precision(precision) => (*precision, 0),
        ExactNumberInfo::PrecisionAndScale(precision, scale) => {
            let Ok(scale) = u32::try_from(*scale) else {
                return Err(crate::errors::Error::forward_refusal(format!(
                    "NUMERIC({precision},{scale}) has a negative scale, which cannot be a count \
                     of minor units."
                )));
            };
            (*precision, scale)
        }
    };

    if precision > MAX_NUMERIC_PRECISION {
        return Err(crate::errors::Error::forward_refusal(format!(
            "NUMERIC({precision},{scale}) needs {precision} digits, and a SQLite INTEGER holds \
             at most {MAX_NUMERIC_PRECISION}. The column would silently become a float, which is \
             what the scaled integer exists to avoid. Reduce the precision, or store the value \
             as TEXT and compare it in the application."
        )));
    }
    if u64::from(scale) > precision {
        return Err(crate::errors::Error::forward_refusal(format!(
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

crate::traits::translator::impl_contextual_translator!(DataType => DataType);
impl crate::traits::translator::TranslatorWithContext for DataType {
    fn translate_with_warnings(
        &self,
        _schema: &Self::Schema,
        options: &crate::options::TranslationContext<'_>,
        _emit: &mut dyn FnMut(crate::warnings::TranslationWarning),
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
// PostgreSQL resolves FLOAT(p) to real up to p of 24 and to double
// precision above it, and refuses p of 54 or more. SQLite has one
// floating type, so the width carries nothing across.
DataType::Float(_)
| DataType::Double(_)
| DataType::DoublePrecision
| DataType::Float8
| DataType::Float4 => Ok(DataType::Real),
// NUMERIC, DECIMAL, and DEC become an INTEGER holding minor units, scaled
// by 10^s, which is the only representation SQLite has that keeps
// decimal arithmetic exact. REAL does not: measured, `sum` over
// 0.10 and 0.20 answers 0.30000000000000004 and `0.1 + 0.2 = 0.3`
// is FALSE. See decision D1.
DataType::Numeric(info) | DataType::Decimal(info) | DataType::Dec(info) => {
    numeric_precision_and_scale(info)?;
    Ok(DataType::Integer(None))
}
// A declared length becomes a column CHECK rather than vanishing,
// and the blank padding CHAR carries is reported. Both live in
// `column.rs`, which is where a column constraint can be attached,
// so all this arm does is refuse a length it could not enforce.
DataType::Char(_)
| DataType::Character(_)
| DataType::Varchar(_)
| DataType::CharacterVarying(_) => {
    character_length(self)?;
    Ok(DataType::Text)
}
// JSON/JSONB, text aliases, and temporal types are stored as TEXT in SQLite.
DataType::JSON
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
            Err(crate::errors::Error::forward_refusal("UUID translation requires specifying a representation (TEXT or BLOB)"
                .to_string()))
        }
    }
}
DataType::Custom(name, ..) => translate_custom_type(name),
unsupported => {
    Err(crate::errors::Error::forward_refusal(format!(
        "The data type {unsupported:?} is not supported"
    )))
}
}
    }
}

/// True when `data_type` is one of PostgreSQL's serial pseudo-types.
///
/// They reach the parser as a custom name rather than a `DataType` variant, so
/// they are recognised by the same name lookup that maps them onto `INTEGER`.
/// Each is shorthand for `integer NOT NULL DEFAULT nextval('...')`, which is
/// why a serial column needs a value source and not merely a type.
pub(crate) fn is_serial_type(data_type: &DataType) -> bool {
    let DataType::Custom(name, _) = data_type else { return false };
    crate::impls::object_name::last_catalog_name(name).is_some_and(|name| {
        matches!(name.as_str(), "serial" | "smallserial" | "bigserial" | "largeserial")
    })
}

/// Maps the PostgreSQL and extension types that reach the parser as a custom
/// name rather than as a `DataType` variant of their own.
fn translate_custom_type(
    name: &sqlparser::ast::ObjectName,
) -> Result<DataType, crate::errors::Error> {
    let custom_type_name = crate::impls::object_name::last_catalog_name(name);

    match custom_type_name.as_deref() {
        Some("serial" | "smallserial" | "bigserial" | "largeserial") => Ok(DataType::Integer(None)),
        Some("countrycode") => Ok(DataType::Text),
        // Three groups that all become BLOB.
        //
        // PostGIS `geometry` and `geography` carry EWKB produced by the
        // SQLiteGIS extension (https://github.com/LucaCappelletti94/sqlitegis),
        // which round-trips through the column. The blob is opaque to SQLite
        // without the extension loaded, see
        // `Pg2SqliteOptions::with_sqlitegis_enabled` for runtime `ST_*`
        // function passthrough.
        //
        // pgvector `vector(N)` and `halfvec(N)` are stored as BLOB in the main
        // table, with a companion vec0 virtual table for indexed KNN search.
        Some(
            "geometry" | "geography" | "cas" | "molecularformula" | "mediatype" | "vector"
            | "halfvec",
        ) => Ok(DataType::Blob(None)),
        _ => {
            Err(crate::errors::Error::forward_refusal(format!(
                "Unknown PostgreSQL custom type {name}"
            )))
        }
    }
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use sql_traits::structs::ParserDB;
    use sqlparser::ast::DataType;

    use crate::prelude::{Pg2SqliteOptions, Translator};

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
