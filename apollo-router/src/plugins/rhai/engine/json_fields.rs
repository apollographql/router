//! Conversion of JSON-bearing request and response fields to and from Rhai values.
//!
//! Every Rhai property accessor for `variables`, `extensions`, `data` and `errors` converts
//! through this module, so there is exactly one place that decides how a JSON field becomes
//! native Rhai `Map`/`Array`/scalar values and how a script's value is written back.
//!
//! A getter converts only the field it reads, into a complete native Rhai value, so scripts get
//! every behavior of Rhai's own collections. A setter converts the complete value Rhai hands it.
//! Rhai calls a setter only when a script assigns the field or changes it through a property,
//! index or method chain, so a plain read never writes back.
//!
//! Tests observe those conversions with [`take_conversions`]. That is how the Rhai
//! compatibility suite asserts that a callback which never touches a field, or which only
//! touches a different field, performs no conversion for it.

mod native;

use rhai::Dynamic;
use rhai::EvalAltResult;
use rhai::serde::from_dynamic;
use rhai::serde::to_dynamic;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::graphql;
use crate::json_ext::Object;
use crate::json_ext::Value;

/// A JSON-bearing field exposed to Rhai scripts.
#[derive(Clone, Copy, Debug)]
pub(crate) enum JsonField {
    RequestVariables,
    RequestExtensions,
    ResponseData,
    ResponseErrors,
    ResponseExtensions,
}

/// How JSON values convert to and from Rhai values.
///
/// Both bridges produce the same Rhai values and the same JSON; the Rhai compatibility suite
/// runs against each.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
// Outside tests only the selected bridge is constructed.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum Bridge {
    /// Build native Rhai values directly from JSON values, and JSON values directly from them.
    Native,
    /// Go through Rhai's serde support, as released Routers do.
    ///
    /// This is the rollback for [`Bridge::Native`] until that passes its release gates. Rolling
    /// back means changing [`SELECTED`]; scripts and configuration do not change.
    Legacy,
}

/// The bridge Router uses.
const SELECTED: Bridge = Bridge::Native;

#[derive(Clone, Copy, Debug)]
enum Direction {
    ToRhai,
    FromRhai,
}

/// The Rust type of a JSON-bearing field, with its [`Bridge::Native`] conversions.
pub(super) trait FieldValue: Serialize + DeserializeOwned {
    fn to_native(&self) -> Result<Dynamic, Box<EvalAltResult>>;

    fn from_native(value: &Dynamic) -> Result<Self, Box<EvalAltResult>>;
}

/// `variables` and `extensions`.
impl FieldValue for Object {
    fn to_native(&self) -> Result<Dynamic, Box<EvalAltResult>> {
        Ok(Dynamic::from_map(native::object_to_map(self)))
    }

    fn from_native(value: &Dynamic) -> Result<Self, Box<EvalAltResult>> {
        native::object_from_rhai(value)
    }
}

/// `data`, which is absent until a response has any.
impl FieldValue for Option<Value> {
    fn to_native(&self) -> Result<Dynamic, Box<EvalAltResult>> {
        Ok(self.as_ref().map_or(Dynamic::UNIT, native::value_to_rhai))
    }

    fn from_native(value: &Dynamic) -> Result<Self, Box<EvalAltResult>> {
        native::object_from_rhai(value).map(|object| Some(Value::Object(object)))
    }
}

/// `errors` holds typed GraphQL errors rather than a JSON value. Their serde implementation
/// defines how an error maps to a Rhai map, including defaults such as an empty `message`, so
/// both bridges use it.
impl FieldValue for Vec<graphql::Error> {
    fn to_native(&self) -> Result<Dynamic, Box<EvalAltResult>> {
        to_dynamic(self)
    }

    fn from_native(value: &Dynamic) -> Result<Self, Box<EvalAltResult>> {
        from_dynamic(value)
    }
}

/// Convert a field's value into a native Rhai value for a property getter.
pub(super) fn to_rhai<T: FieldValue>(
    field: JsonField,
    value: &T,
) -> Result<Dynamic, Box<EvalAltResult>> {
    record(field, Direction::ToRhai);
    match selected() {
        Bridge::Native => value.to_native(),
        Bridge::Legacy => to_dynamic(value),
    }
}

/// Convert a script's value back into a field's value for a property setter.
pub(super) fn from_rhai<T: FieldValue>(
    field: JsonField,
    value: &Dynamic,
) -> Result<T, Box<EvalAltResult>> {
    record(field, Direction::FromRhai);
    match selected() {
        Bridge::Native => T::from_native(value),
        Bridge::Legacy => from_dynamic(value),
    }
}

#[cfg(not(test))]
fn selected() -> Bridge {
    SELECTED
}

#[cfg(not(test))]
fn record(_field: JsonField, _direction: Direction) {}

#[cfg(test)]
pub(crate) use counting::*;

#[cfg(test)]
mod counting {
    use std::cell::Cell;

    use super::Bridge;
    use super::Direction;
    use super::JsonField;
    use super::SELECTED;

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
        static BRIDGE: Cell<Bridge> = const { Cell::new(SELECTED) };
    }

    /// Return and reset the conversions recorded on the current thread.
    ///
    /// Rhai callbacks run synchronously on the thread polling the pipeline, so a test on a
    /// current-thread runtime observes every conversion its callbacks perform.
    pub(crate) fn take_conversions() -> Conversions {
        CONVERSIONS.take()
    }

    impl Bridge {
        pub(crate) const ALL: [Bridge; 2] = [Bridge::Native, Bridge::Legacy];

        /// Convert with this bridge on the current thread until the returned guard is dropped.
        pub(crate) fn select(self) -> SelectedBridge {
            SelectedBridge(BRIDGE.replace(self))
        }
    }

    /// Restores the previously selected bridge when dropped.
    pub(crate) struct SelectedBridge(Bridge);

    impl Drop for SelectedBridge {
        fn drop(&mut self) {
            BRIDGE.set(self.0);
        }
    }

    pub(super) fn selected() -> Bridge {
        BRIDGE.get()
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
