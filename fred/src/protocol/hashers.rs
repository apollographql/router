use crate::types::Value;
use redis_protocol::redis_keyslot;

pub fn hash_value(value: &Value) -> Option<u16> {
  Some(match value {
    Value::String(s) => redis_keyslot(s.as_bytes()),
    Value::Bytes(b) => redis_keyslot(b),
    Value::Integer(i) => redis_keyslot(i.to_string().as_bytes()),
    Value::Double(f) => redis_keyslot(f.to_string().as_bytes()),
    Value::Null => redis_keyslot(b"nil"),
    Value::Boolean(b) => redis_keyslot(b.to_string().as_bytes()),
    _ => return None,
  })
}

pub fn read_key(value: &Value) -> Option<&[u8]> {
  match value {
    Value::String(s) => Some(s.as_bytes()),
    Value::Bytes(b) => Some(b),
    _ => None,
  }
}

fn hash_key(value: &Value) -> Option<u16> {
  read_key(value).map(redis_keyslot)
}

/// A cluster hashing policy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClusterHash {
  /// Hash the first string or bytes value in the arguments. (Default)
  FirstKey,
  /// Hash the first argument regardless of type.
  FirstValue,
  /// Use a random node in the cluster.
  Random,
  /// Hash the value with the provided offset in the arguments array.
  Offset(usize),
  /// Provide a custom hash slot value.
  Custom(u16),
}

impl Default for ClusterHash {
  fn default() -> Self {
    ClusterHash::FirstKey
  }
}

impl From<Option<u16>> for ClusterHash {
  fn from(hash_slot: Option<u16>) -> Self {
    match hash_slot {
      Some(slot) => ClusterHash::Custom(slot),
      None => ClusterHash::FirstKey,
    }
  }
}

impl From<u16> for ClusterHash {
  fn from(slot: u16) -> Self {
    ClusterHash::Custom(slot)
  }
}

impl From<&str> for ClusterHash {
  fn from(d: &str) -> Self {
    ClusterHash::Custom(redis_keyslot(d.as_bytes()))
  }
}

impl From<&[u8]> for ClusterHash {
  fn from(d: &[u8]) -> Self {
    ClusterHash::Custom(redis_keyslot(d))
  }
}

impl ClusterHash {
  /// Hash the provided arguments.
  pub fn hash(&self, args: &[Value]) -> Option<u16> {
    match self {
      ClusterHash::FirstValue => args.first().and_then(hash_value),
      ClusterHash::FirstKey => args.iter().find_map(hash_key),
      ClusterHash::Random => None,
      ClusterHash::Offset(idx) => args.get(*idx).and_then(hash_value),
      ClusterHash::Custom(val) => Some(*val),
    }
  }

  /// Find the key to hash with the provided arguments.
  pub fn find_key<'a>(&self, args: &'a [Value]) -> Option<&'a [u8]> {
    match self {
      ClusterHash::FirstValue => args.first().and_then(read_key),
      ClusterHash::FirstKey => args.iter().find_map(read_key),
      ClusterHash::Offset(idx) => args.get(*idx).and_then(read_key),
      ClusterHash::Random | ClusterHash::Custom(_) => None,
    }
  }
}
