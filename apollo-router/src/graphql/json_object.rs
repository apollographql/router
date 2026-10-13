//! Object-shaped JSON values, as the GraphQL request, response, and error types carry them.

use apollo_json::JsonKind;
use apollo_json::NewValue;
use apollo_json::ValueBuilder;

use crate::json_ext::Value;

/// A JSON object with no members.
pub(crate) fn empty_object() -> Value {
    ValueBuilder::new().seal()
}

/// Whether `value` carries no object members. A value of any other shape counts as empty.
pub(crate) fn is_empty_object(value: &Value) -> bool {
    value.len().unwrap_or(0) == 0
}

/// `object` with `value` written at `key`, replacing any member already there.
pub(crate) fn insert_member<'v>(object: Value, key: &str, value: impl Into<NewValue<'v>>) -> Value {
    let mut builder = edit_object(object);
    set_member(&mut builder, key, value);
    builder.seal()
}

/// A builder over a copy of `object`, or over a fresh empty object when `object` holds
/// another shape.
fn edit_object(object: Value) -> ValueBuilder {
    if object.kind() == JsonKind::Object {
        object.compact().edit()
    } else {
        ValueBuilder::new()
    }
}

fn set_member<'v>(builder: &mut ValueBuilder, key: &str, value: impl Into<NewValue<'v>>) {
    builder
        .set(key, value)
        .expect("an object root accepts any key holding a finite number");
}

/// Collects the members of a JSON object across the calls to a builder's setters.
#[derive(Default)]
pub(crate) struct ObjectAccumulator(Option<Value>);

impl ObjectAccumulator {
    /// Adds one member, replacing any member already at `key`.
    pub(crate) fn insert<'v>(&mut self, key: impl Into<String>, value: impl Into<NewValue<'v>>) {
        let object = self.0.take().unwrap_or_else(empty_object);
        self.0 = Some(insert_member(object, key.into().as_str(), value));
    }

    /// Adds every member of `members`, replacing members already at the same keys.
    pub(crate) fn extend(&mut self, members: Value) {
        match self.0.take() {
            None => self.0 = Some(members),
            Some(object) => {
                let mut builder = edit_object(object);
                for (key, value) in members.object_iter() {
                    set_member(&mut builder, &key, value);
                }
                self.0 = Some(builder.seal());
            }
        }
    }

    /// The collected members as a JSON object.
    pub(crate) fn build(self) -> Value {
        self.0.unwrap_or_else(empty_object)
    }
}
