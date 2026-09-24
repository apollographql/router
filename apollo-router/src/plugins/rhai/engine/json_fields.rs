//! Conversion of JSON-bearing request and response fields to and from Rhai values.
//!
//! Every Rhai property accessor for `variables`, `extensions`, `data` and `errors` converts
//! through this module, so there is exactly one place that decides how a JSON field becomes
//! native Rhai `Map`/`Array`/scalar values and how a script's value is written back.
//!
//! Tests observe those conversions with [`take_conversions`]. That is how the Rhai
//! compatibility suite asserts that a callback which never touches a field, or which only
//! touches a different field, performs no conversion for it.

use rhai::Dynamic;
use rhai::EvalAltResult;
use rhai::serde::from_dynamic;
use rhai::serde::to_dynamic;
use serde::Serialize;
use serde::de::DeserializeOwned;

/// A JSON-bearing field exposed to Rhai scripts.
#[derive(Clone, Copy, Debug)]
pub(crate) enum JsonField {
    RequestVariables,
    RequestExtensions,
    ResponseData,
    ResponseErrors,
    ResponseExtensions,
}

#[derive(Clone, Copy, Debug)]
enum Direction {
    ToRhai,
    FromRhai,
}

/// Convert a field's value into a native Rhai value for a property getter.
pub(super) fn to_rhai<T: Serialize>(
    field: JsonField,
    value: &T,
) -> Result<Dynamic, Box<EvalAltResult>> {
    record(field, Direction::ToRhai);
    to_dynamic(value)
}

/// Convert a script's value back into a field's value for a property setter.
pub(super) fn from_rhai<T: DeserializeOwned>(
    field: JsonField,
    value: &Dynamic,
) -> Result<T, Box<EvalAltResult>> {
    record(field, Direction::FromRhai);
    from_dynamic(value)
}

#[cfg(not(test))]
fn record(_field: JsonField, _direction: Direction) {}

#[cfg(test)]
pub(crate) use counting::*;

#[cfg(test)]
mod counting {
    use std::cell::Cell;

    use super::Direction;
    use super::JsonField;

    /// Number of conversions per JSON field, in one direction.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub(crate) struct FieldCounts {
        pub(crate) request_variables: usize,
        pub(crate) request_extensions: usize,
        pub(crate) response_data: usize,
        pub(crate) response_errors: usize,
        pub(crate) response_extensions: usize,
    }

    /// Conversions performed since the last call to [`take_conversions`].
    ///
    /// `to_rhai` counts getter conversions; `from_rhai` counts setter write-backs.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub(crate) struct Conversions {
        pub(crate) to_rhai: FieldCounts,
        pub(crate) from_rhai: FieldCounts,
    }

    thread_local! {
        static CONVERSIONS: Cell<Conversions> = Cell::new(Conversions::default());
    }

    /// Return and reset the conversions recorded on the current thread.
    ///
    /// Rhai callbacks run synchronously on the thread polling the pipeline, so a test on a
    /// current-thread runtime observes every conversion its callbacks perform.
    pub(crate) fn take_conversions() -> Conversions {
        CONVERSIONS.take()
    }

    pub(super) fn record(field: JsonField, direction: Direction) {
        let mut conversions = CONVERSIONS.get();
        let counts = match direction {
            Direction::ToRhai => &mut conversions.to_rhai,
            Direction::FromRhai => &mut conversions.from_rhai,
        };
        let count = match field {
            JsonField::RequestVariables => &mut counts.request_variables,
            JsonField::RequestExtensions => &mut counts.request_extensions,
            JsonField::ResponseData => &mut counts.response_data,
            JsonField::ResponseErrors => &mut counts.response_errors,
            JsonField::ResponseExtensions => &mut counts.response_extensions,
        };
        *count += 1;
        CONVERSIONS.set(conversions);
    }
}
