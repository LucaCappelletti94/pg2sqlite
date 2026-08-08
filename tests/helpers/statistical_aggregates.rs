//! Registers the nine PostgreSQL statistical aggregates on a `rusqlite`
//! connection, which SQLite does not provide.
//!
//! Include in a test binary with:
//!
//! ```rust,ignore
//! #[path = "helpers/statistical_aggregates.rs"]
//! mod statistical_aggregates;
//! use statistical_aggregates::{STATISTICAL_AGGREGATES, register_statistical_aggregates};
//! ```
//!
//! The translator refuses these names unless the caller declares them, so a
//! test that wants a number out of one has to supply the implementation. The
//! same list is what `with_user_defined_functions` is handed, which keeps the
//! declaration and the registration from drifting apart.
//!
//! Each accumulator keeps the values it was given and computes the answer in
//! two passes over them. That is exact rather than merely stable, it makes
//! `inverse` a removal instead of an algebraic undo, and the cost of holding a
//! test group in memory is irrelevant here. A caller writing this for
//! production would use Welford's method instead and give up the exactness.
//!
//! Registration goes through `create_window_function`, so the same
//! registration answers both a plain aggregate call and one carrying `OVER`.

// A group's row count crosses into `f64` to be divided by, which is what
// every one of these aggregates is defined as.
#![allow(clippy::cast_precision_loss)]

use rusqlite::{
    Connection, Result,
    functions::{Aggregate, Context, FunctionFlags, WindowAggregate},
};

/// What each name is registered as. One table so the list a caller declares
/// and the implementations a connection carries cannot drift apart.
const UNIVARIATE: &[(&str, Univariate)] = &[
    ("var_pop", Univariate::VarPop),
    ("var_samp", Univariate::VarSamp),
    ("variance", Univariate::VarSamp),
    ("stddev_pop", Univariate::StddevPop),
    ("stddev", Univariate::StddevSamp),
    ("stddev_samp", Univariate::StddevSamp),
];

const BIVARIATE: &[(&str, Bivariate)] = &[
    ("covar_pop", Bivariate::CovarPop),
    ("covar_samp", Bivariate::CovarSamp),
    ("corr", Bivariate::Corr),
];

/// Every aggregate this module registers, in the spelling the translator sees.
pub const STATISTICAL_AGGREGATES: &[&str] = &[
    "var_pop",
    "var_samp",
    "variance",
    "stddev_pop",
    "stddev",
    "stddev_samp",
    "covar_pop",
    "covar_samp",
    "corr",
];

/// Registers all nine on `connection`.
///
/// # Errors
///
/// Propagates a `rusqlite` registration failure.
pub fn register_statistical_aggregates(connection: &Connection) -> Result<()> {
    let flags = FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC;
    for (name, kind) in UNIVARIATE {
        connection.create_window_function(name, 1, flags, *kind)?;
    }
    for (name, kind) in BIVARIATE {
        connection.create_window_function(name, 2, flags, *kind)?;
    }
    Ok(())
}

/// The declared list is what the registration table registers, in order.
/// A name added to one and not the other would translate but not run, or run
/// but not translate.
#[test]
fn the_declared_list_is_what_gets_registered() {
    let registered: Vec<&str> = UNIVARIATE
        .iter()
        .map(|(name, _)| *name)
        .chain(BIVARIATE.iter().map(|(name, _)| *name))
        .collect();
    assert_eq!(registered, STATISTICAL_AGGREGATES);
}

/// Mean and sum of squared deviations, or `None` when there is nothing to
/// average.
fn moments(values: &[f64]) -> Option<(f64, f64)> {
    if values.is_empty() {
        return None;
    }
    let count = values.len() as f64;
    let mean = values.iter().sum::<f64>() / count;
    Some((mean, values.iter().map(|value| (value - mean) * (value - mean)).sum()))
}

/// Whether two values are the same one, bit for bit.
///
/// A window's inverse step removes a value that a step put in, so the match
/// wanted here is identity rather than nearness, and comparing the bits says
/// that without inviting a tolerance that would remove the wrong row.
fn same_value(left: f64, right: f64) -> bool {
    left.to_bits() == right.to_bits()
}

/// The four aggregates over one column.
#[derive(Clone, Copy)]
enum Univariate {
    VarPop,
    VarSamp,
    StddevPop,
    StddevSamp,
}

impl Univariate {
    /// PostgreSQL takes the population forms over one row and the sample forms
    /// over two, and answers NULL below that.
    fn answer(self, values: &[f64]) -> Option<f64> {
        let (_, deviations) = moments(values)?;
        let count = values.len() as f64;
        match self {
            Self::VarPop => Some(deviations / count),
            Self::StddevPop => Some((deviations / count).sqrt()),
            Self::VarSamp if values.len() >= 2 => Some(deviations / (count - 1.0)),
            Self::StddevSamp if values.len() >= 2 => Some((deviations / (count - 1.0)).sqrt()),
            Self::VarSamp | Self::StddevSamp => None,
        }
    }
}

impl Aggregate<Vec<f64>, Option<f64>> for Univariate {
    fn init(&self, _: &mut Context<'_>) -> Result<Vec<f64>> {
        Ok(Vec::new())
    }

    fn step(&self, context: &mut Context<'_>, accumulator: &mut Vec<f64>) -> Result<()> {
        if let Some(value) = context.get::<Option<f64>>(0)? {
            accumulator.push(value);
        }
        Ok(())
    }

    fn finalize(&self, _: &mut Context<'_>, accumulator: Option<Vec<f64>>) -> Result<Option<f64>> {
        Ok(accumulator.and_then(|values| self.answer(&values)))
    }
}

impl WindowAggregate<Vec<f64>, Option<f64>> for Univariate {
    fn value(&self, accumulator: Option<&mut Vec<f64>>) -> Result<Option<f64>> {
        Ok(accumulator.and_then(|values| self.answer(values)))
    }

    fn inverse(&self, context: &mut Context<'_>, accumulator: &mut Vec<f64>) -> Result<()> {
        if let Some(value) = context.get::<Option<f64>>(0)?
            && let Some(position) = accumulator.iter().position(|held| same_value(*held, value))
        {
            accumulator.remove(position);
        }
        Ok(())
    }
}

/// The three aggregates over two columns, all taken over the complete pairs.
#[derive(Clone, Copy)]
enum Bivariate {
    CovarPop,
    CovarSamp,
    Corr,
}

impl Bivariate {
    fn answer(self, pairs: &[(f64, f64)]) -> Option<f64> {
        if pairs.is_empty() {
            return None;
        }
        let count = pairs.len() as f64;
        let mean_x = pairs.iter().map(|(x, _)| x).sum::<f64>() / count;
        let mean_y = pairs.iter().map(|(_, y)| y).sum::<f64>() / count;
        let product = pairs.iter().map(|(x, y)| (x - mean_x) * (y - mean_y)).sum::<f64>();
        match self {
            Self::CovarPop => Some(product / count),
            Self::CovarSamp if pairs.len() >= 2 => Some(product / (count - 1.0)),
            Self::CovarSamp => None,
            // PostgreSQL answers NULL rather than dividing by zero when either
            // side does not vary, measured on 17 over two rows sharing an x.
            Self::Corr => {
                let spread_x = pairs.iter().map(|(x, _)| (x - mean_x) * (x - mean_x)).sum::<f64>();
                let spread_y = pairs.iter().map(|(_, y)| (y - mean_y) * (y - mean_y)).sum::<f64>();
                if spread_x == 0.0 || spread_y == 0.0 {
                    None
                } else {
                    Some(product / (spread_x * spread_y).sqrt())
                }
            }
        }
    }
}

impl Aggregate<Vec<(f64, f64)>, Option<f64>> for Bivariate {
    fn init(&self, _: &mut Context<'_>) -> Result<Vec<(f64, f64)>> {
        Ok(Vec::new())
    }

    fn step(&self, context: &mut Context<'_>, accumulator: &mut Vec<(f64, f64)>) -> Result<()> {
        if let (Some(x), Some(y)) = (context.get::<Option<f64>>(0)?, context.get::<Option<f64>>(1)?)
        {
            accumulator.push((x, y));
        }
        Ok(())
    }

    fn finalize(
        &self,
        _: &mut Context<'_>,
        accumulator: Option<Vec<(f64, f64)>>,
    ) -> Result<Option<f64>> {
        Ok(accumulator.and_then(|pairs| self.answer(&pairs)))
    }
}

impl WindowAggregate<Vec<(f64, f64)>, Option<f64>> for Bivariate {
    fn value(&self, accumulator: Option<&mut Vec<(f64, f64)>>) -> Result<Option<f64>> {
        Ok(accumulator.and_then(|pairs| self.answer(pairs)))
    }

    fn inverse(&self, context: &mut Context<'_>, accumulator: &mut Vec<(f64, f64)>) -> Result<()> {
        if let (Some(x), Some(y)) = (context.get::<Option<f64>>(0)?, context.get::<Option<f64>>(1)?)
            && let Some(position) = accumulator
                .iter()
                .position(|(held_x, held_y)| same_value(*held_x, x) && same_value(*held_y, y))
        {
            accumulator.remove(position);
        }
        Ok(())
    }
}
