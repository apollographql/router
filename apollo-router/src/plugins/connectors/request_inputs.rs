use serde_json_bytes::ByteString;
use serde_json_bytes::Map;
use serde_json_bytes::Value;

pub(super) const PARENT_PREFIX: &str = "$this";

#[derive(Debug, Default)]
pub(super) struct RequestInputs {
    pub(super) arguments: Map<ByteString, Value>,
    pub(super) parent: Map<ByteString, Value>,
}

impl RequestInputs {
    pub(super) fn merge(&self) -> Value {
        let mut new = Map::new();
        new.extend(self.parent.clone());
        new.extend(self.arguments.clone());
        // if parent types are shadowed by arguments, we can use `$this.` to access them
        new.insert(PARENT_PREFIX, Value::Object(self.parent.clone()));
        Value::Object(new)
    }
}
