use blake3::Hasher;
use serde::Serialize;
use serde::Serializer;
use serde::ser::SerializeMap;
use serde::ser::SerializeSeq;
use serde::ser::SerializeStruct;
use serde::ser::SerializeStructVariant;
use serde::ser::SerializeTuple;
use serde::ser::SerializeTupleStruct;
use serde::ser::SerializeTupleVariant;
use serde::ser::{self};

/// A serializer that hashes the data instead of serializing it.
pub(crate) struct Blake3Serializer<'a> {
    /// A reference to the digest that will accumulate the data.
    pub(crate) hasher: &'a mut Hasher,
}

impl<'a> Blake3Serializer<'a> {
    pub(crate) fn new(hasher: &'a mut Hasher) -> Self {
        Self { hasher }
    }
}

/// Possible errors during serialization.
#[derive(Debug, PartialEq, Eq, Clone)]
pub(crate) enum Error {
    /// Custom `serde` error.
    CustomError(String),
}

impl ser::Error for Error {
    fn custom<T: std::fmt::Display>(msg: T) -> Self {
        Self::CustomError(format!("{}", msg))
    }
}

impl ser::StdError for Error {}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

// Implement `serialize_$ty` for int types
macro_rules! impl_trivial_serialize {
    ($method_name:ident , $t:ty) => {
        fn $method_name(self, v: $t) -> Result<Self::Ok, Self::Error> {
            self.hasher.update(&v.to_be_bytes());
            Ok(())
        }
    };
}

impl<'a> Serializer for Blake3Serializer<'a> {
    type Ok = ();
    type Error = Error;
    type SerializeSeq = Self;
    type SerializeTuple = Self;
    type SerializeTupleStruct = Self;
    type SerializeTupleVariant = Self;
    type SerializeMap = Self;
    type SerializeStruct = Self;
    type SerializeStructVariant = Self;

    fn serialize_bool(self, v: bool) -> Result<Self::Ok, Self::Error> {
        self.hasher.update(&(v as u8).to_be_bytes());
        Ok(())
    }

    impl_trivial_serialize!(serialize_i8, i8);
    impl_trivial_serialize!(serialize_i16, i16);
    impl_trivial_serialize!(serialize_i32, i32);
    impl_trivial_serialize!(serialize_i64, i64);

    impl_trivial_serialize!(serialize_u8, u8);
    impl_trivial_serialize!(serialize_u16, u16);
    impl_trivial_serialize!(serialize_u32, u32);
    impl_trivial_serialize!(serialize_u64, u64);

    impl_trivial_serialize!(serialize_f32, f32);
    impl_trivial_serialize!(serialize_f64, f64);

    fn serialize_char(self, v: char) -> Result<Self::Ok, Self::Error> {
        // `char` is always at most 4 bytes, regardless of the platform,
        // so this conversion is safe.
        self.hasher.update(&u64::from(v).to_be_bytes());
        Ok(())
    }

    fn serialize_str(self, v: &str) -> Result<Self::Ok, Self::Error> {
        self.hasher.update(v.as_ref());
        Ok(())
    }

    fn serialize_bytes(self, v: &[u8]) -> Result<Self::Ok, Self::Error> {
        self.hasher.update(v);
        Ok(())
    }

    fn serialize_none(self) -> Result<Self::Ok, Self::Error> {
        self.hasher.update(&[0]);
        Ok(())
    }

    fn serialize_some<V: ?Sized + Serialize>(self, value: &V) -> Result<Self::Ok, Self::Error> {
        self.hasher.update(&[1]);
        value.serialize(self)
    }

    fn serialize_unit(self) -> Result<Self::Ok, Self::Error> {
        Ok(())
    }

    fn serialize_unit_struct(self, _name: &'static str) -> Result<Self::Ok, Self::Error> {
        Ok(())
    }

    fn serialize_unit_variant(
        self,
        _name: &'static str,
        variant_index: u32,
        _variant: &'static str,
    ) -> Result<Self::Ok, Self::Error> {
        self.hasher.update(&variant_index.to_be_bytes());
        Ok(())
    }

    fn serialize_newtype_struct<V: ?Sized + Serialize>(
        self,
        _name: &'static str,
        value: &V,
    ) -> Result<Self::Ok, Self::Error> {
        value.serialize(self)
    }

    fn serialize_newtype_variant<V: ?Sized + Serialize>(
        self,
        _name: &'static str,
        variant_index: u32,
        _variant: &'static str,
        value: &V,
    ) -> Result<Self::Ok, Self::Error> {
        self.hasher.update(&variant_index.to_be_bytes());

        value.serialize(self)
    }

    fn serialize_seq(self, len: Option<usize>) -> Result<Self::SerializeSeq, Self::Error> {
        if let Some(len) = len {
            self.hasher.update(&len.to_be_bytes());
        }
        Ok(self)
    }

    fn serialize_tuple(self, len: usize) -> Result<Self::SerializeTuple, Self::Error> {
        self.hasher.update(&len.to_be_bytes());
        Ok(self)
    }

    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        len: usize,
    ) -> Result<Self::SerializeTupleStruct, Self::Error> {
        self.hasher.update(&len.to_be_bytes());

        Ok(self)
    }

    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        variant_index: u32,
        _variant: &'static str,
        len: usize,
    ) -> Result<Self::SerializeTupleVariant, Self::Error> {
        self.hasher.update(&variant_index.to_be_bytes());
        self.hasher.update(&len.to_be_bytes());
        Ok(self)
    }

    fn serialize_map(self, len: Option<usize>) -> Result<Self::SerializeMap, Self::Error> {
        if let Some(len) = len {
            self.hasher.update(&len.to_be_bytes());
        }
        Ok(self)
    }

    fn serialize_struct(
        self,
        _name: &'static str,
        len: usize,
    ) -> Result<Self::SerializeStruct, Self::Error> {
        self.hasher.update(&len.to_be_bytes());
        Ok(self)
    }

    fn serialize_struct_variant(
        self,
        _name: &'static str,
        variant_index: u32,
        _variant: &'static str,
        len: usize,
    ) -> Result<Self::SerializeStructVariant, Self::Error> {
        self.hasher.update(&variant_index.to_be_bytes());
        self.hasher.update(&len.to_be_bytes());
        Ok(self)
    }

    fn is_human_readable(&self) -> bool {
        false
    }
}

impl<'a> SerializeSeq for Blake3Serializer<'a> {
    type Ok = ();
    type Error = Error;

    fn serialize_element<V: ?Sized + Serialize>(&mut self, value: &V) -> Result<Self::Ok, Error> {
        value.serialize(Blake3Serializer {
            hasher: self.hasher,
        })?;
        Ok(())
    }

    fn end(self) -> Result<Self::Ok, Error> {
        Ok(())
    }
}

impl<'a> SerializeTuple for Blake3Serializer<'a> {
    type Ok = ();
    type Error = Error;

    fn serialize_element<V: ?Sized + Serialize>(&mut self, value: &V) -> Result<Self::Ok, Error> {
        value.serialize(Blake3Serializer {
            hasher: self.hasher,
        })?;
        Ok(())
    }

    fn end(self) -> Result<Self::Ok, Error> {
        Ok(())
    }
}

impl<'a> SerializeTupleStruct for Blake3Serializer<'a> {
    type Ok = ();
    type Error = Error;

    fn serialize_field<V: ?Sized + Serialize>(&mut self, value: &V) -> Result<Self::Ok, Error> {
        value.serialize(Blake3Serializer {
            hasher: self.hasher,
        })?;
        Ok(())
    }

    fn end(self) -> Result<Self::Ok, Error> {
        Ok(())
    }
}

impl<'a> SerializeTupleVariant for Blake3Serializer<'a> {
    type Ok = ();
    type Error = Error;

    fn serialize_field<V: ?Sized + Serialize>(&mut self, value: &V) -> Result<Self::Ok, Error> {
        value.serialize(Blake3Serializer {
            hasher: self.hasher,
        })?;
        Ok(())
    }

    fn end(self) -> Result<Self::Ok, Error> {
        Ok(())
    }
}

impl<'a> SerializeMap for Blake3Serializer<'a> {
    type Ok = ();
    type Error = Error;

    fn serialize_key<K: ?Sized + Serialize>(&mut self, key: &K) -> Result<Self::Ok, Error> {
        key.serialize(Blake3Serializer {
            hasher: self.hasher,
        })?;
        Ok(())
    }

    fn serialize_value<V: ?Sized + Serialize>(&mut self, value: &V) -> Result<Self::Ok, Error> {
        value.serialize(Blake3Serializer {
            hasher: self.hasher,
        })?;
        Ok(())
    }

    fn end(self) -> Result<Self::Ok, Error> {
        Ok(())
    }
}

impl<'a> SerializeStruct for Blake3Serializer<'a> {
    type Ok = ();
    type Error = Error;

    fn serialize_field<V: ?Sized + Serialize>(
        &mut self,
        key: &'static str,
        value: &V,
    ) -> Result<Self::Ok, Error> {
        self.hasher.update(key.as_bytes());
        value.serialize(Blake3Serializer {
            hasher: self.hasher,
        })?;
        Ok(())
    }

    fn end(self) -> Result<Self::Ok, Error> {
        Ok(())
    }
}

impl<'a> SerializeStructVariant for Blake3Serializer<'a> {
    type Ok = ();
    type Error = Error;

    fn serialize_field<V: ?Sized + Serialize>(
        &mut self,
        key: &'static str,
        value: &V,
    ) -> Result<Self::Ok, Error> {
        self.hasher.update(key.as_bytes());
        value.serialize(Blake3Serializer {
            hasher: self.hasher,
        })?;
        Ok(())
    }

    fn end(self) -> Result<Self::Ok, Error> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Number;
    use serde_json_bytes::ByteString;
    use serde_json_bytes::Value;

    use super::*;
    use crate::json_ext::Object;

    #[test]
    fn test_bytestring_map() {
        let mut obj = Object::new();
        obj.insert(
            ByteString::from("test".to_string()),
            Value::String("test".to_string().into()),
        );
        obj.insert(
            ByteString::from("representations".to_string()),
            Value::Array(vec![
                Value::String("test_value".to_string().into()),
                Value::Number(Number::from_f64(1.5).unwrap()),
            ]),
        );
        let mut hasher = blake3::Hasher::new();
        let serializer = Blake3Serializer::new(&mut hasher);

        obj.serialize(serializer).unwrap();

        let first_hash = hasher.finalize().to_hex().to_string();
        insta::assert_snapshot!(first_hash);

        let mut obj = Object::new();
        obj.insert(
            ByteString::from("test".to_string()),
            Value::String("test".to_string().into()),
        );
        obj.insert(
            ByteString::from("representations".to_string()),
            // Change order
            Value::Array(vec![
                Value::Number(Number::from_f64(1.5).unwrap()),
                Value::String("test_value".to_string().into()),
            ]),
        );
        let mut hasher = blake3::Hasher::new();
        let serializer = Blake3Serializer::new(&mut hasher);

        obj.serialize(serializer).unwrap();

        let second_hash = hasher.finalize().to_hex().to_string();
        insta::assert_snapshot!(second_hash);

        assert!(first_hash != second_hash);
    }
}
