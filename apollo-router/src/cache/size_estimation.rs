use std::fmt::Debug;
use std::fmt::Display;
use std::fmt::Formatter;

use serde::ser;
use serde::ser::SerializeMap;
use serde::ser::SerializeSeq;
use serde::ser::SerializeStruct;
use serde::ser::SerializeStructVariant;
use serde::ser::SerializeTuple;
use serde::ser::SerializeTupleStruct;
use serde::ser::SerializeTupleVariant;
use serde::Serialize;

pub(crate) fn estimate_size<T: Serialize>(s: &T) -> usize {
    let ser = s
        .serialize(CountingSerializer::default())
        .expect("mut be able to serialize");
    ser.count
}

pub(crate) struct Error;

impl Debug for Error {
    fn fmt(&self, _f: &mut Formatter<'_>) -> std::fmt::Result {
        unreachable!()
    }
}

impl Display for Error {
    fn fmt(&self, _f: &mut Formatter<'_>) -> std::fmt::Result {
        unreachable!()
    }
}

impl std::error::Error for Error {}

impl ser::Error for Error {
    fn custom<T: Display>(_msg: T) -> Self {
        unreachable!()
    }
}

/// This is a special serializer that doesn't store the serialized data, instead it counts the bytes
/// Yes, it's inaccurate, but we're looking for something that is relatively cheap to compute.
/// It doesn't take into account shared datastructures occurring multiple times and will give the
/// full estimated serialized cost.
#[derive(Default, Debug)]
struct CountingSerializer {
    count: usize,
}

impl ser::Serializer for CountingSerializer {
    type Ok = Self;
    type Error = Error;
    type SerializeSeq = Self;
    type SerializeTuple = Self;
    type SerializeTupleStruct = Self;
    type SerializeTupleVariant = Self;
    type SerializeMap = Self;
    type SerializeStruct = Self;
    type SerializeStructVariant = Self;

    fn serialize_bool(mut self, _v: bool) -> Result<Self::Ok, Self::Error> {
        self.count += std::mem::size_of::<bool>();
        Ok(self)
    }

    fn serialize_i8(mut self, _v: i8) -> Result<Self::Ok, Self::Error> {
        self.count += std::mem::size_of::<i8>();
        Ok(self)
    }

    fn serialize_i16(mut self, _v: i16) -> Result<Self::Ok, Self::Error> {
        self.count += std::mem::size_of::<i16>();
        Ok(self)
    }

    fn serialize_i32(mut self, _v: i32) -> Result<Self::Ok, Self::Error> {
        self.count += std::mem::size_of::<i32>();
        Ok(self)
    }

    fn serialize_i64(mut self, _v: i64) -> Result<Self::Ok, Self::Error> {
        self.count += std::mem::size_of::<i64>();
        Ok(self)
    }

    fn serialize_u8(mut self, _v: u8) -> Result<Self::Ok, Self::Error> {
        self.count += std::mem::size_of::<u8>();
        Ok(self)
    }

    fn serialize_u16(mut self, _v: u16) -> Result<Self::Ok, Self::Error> {
        self.count += std::mem::size_of::<u8>();
        Ok(self)
    }

    fn serialize_u32(mut self, _v: u32) -> Result<Self::Ok, Self::Error> {
        self.count += std::mem::size_of::<u32>();
        Ok(self)
    }

    fn serialize_u64(mut self, _v: u64) -> Result<Self::Ok, Self::Error> {
        self.count += std::mem::size_of::<u64>();
        Ok(self)
    }

    fn serialize_f32(mut self, _v: f32) -> Result<Self::Ok, Self::Error> {
        self.count += std::mem::size_of::<f32>();
        Ok(self)
    }

    fn serialize_f64(mut self, _v: f64) -> Result<Self::Ok, Self::Error> {
        self.count += std::mem::size_of::<f64>();
        Ok(self)
    }

    fn serialize_char(mut self, _v: char) -> Result<Self::Ok, Self::Error> {
        self.count += std::mem::size_of::<char>();
        Ok(self)
    }

    fn serialize_str(mut self, v: &str) -> Result<Self::Ok, Self::Error> {
        //ptr + 8 bytes length + 8 bytes capacity
        self.count += 24 + v.len();
        Ok(self)
    }

    fn serialize_bytes(mut self, v: &[u8]) -> Result<Self::Ok, Self::Error> {
        self.count += v.len();
        Ok(self)
    }

    fn serialize_none(self) -> Result<Self::Ok, Self::Error> {
        Ok(self)
    }

    fn serialize_some<T>(self, value: &T) -> Result<Self::Ok, Self::Error>
    where
        T: ?Sized + Serialize,
    {
        Ok(value.serialize(self).expect("failed to serialize"))
    }

    fn serialize_unit(self) -> Result<Self::Ok, Self::Error> {
        Ok(self)
    }

    fn serialize_unit_struct(self, _name: &'static str) -> Result<Self::Ok, Self::Error> {
        Ok(self)
    }

    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
    ) -> Result<Self::Ok, Self::Error> {
        Ok(self)
    }

    fn serialize_newtype_struct<T>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<Self::Ok, Self::Error>
    where
        T: ?Sized + Serialize,
    {
        Ok(value.serialize(self).expect("failed to serialize"))
    }

    fn serialize_newtype_variant<T>(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        value: &T,
    ) -> Result<Self::Ok, Self::Error>
    where
        T: ?Sized + Serialize,
    {
        Ok(value.serialize(self).expect("failed to serialize"))
    }

    fn serialize_seq(self, _len: Option<usize>) -> Result<Self::SerializeSeq, Self::Error> {
        Ok(self)
    }

    fn serialize_tuple(self, _len: usize) -> Result<Self::SerializeTuple, Self::Error> {
        Ok(self)
    }

    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleStruct, Self::Error> {
        Ok(self)
    }

    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleVariant, Self::Error> {
        Ok(self)
    }

    fn serialize_map(self, _len: Option<usize>) -> Result<Self::SerializeMap, Self::Error> {
        Ok(self)
    }

    fn serialize_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStruct, Self::Error> {
        Ok(self)
    }

    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStructVariant, Self::Error> {
        Ok(self)
    }
}
impl SerializeStructVariant for CountingSerializer {
    type Ok = Self;
    type Error = Error;

    fn serialize_field<T>(&mut self, _key: &'static str, value: &T) -> Result<(), Self::Error>
    where
        T: ?Sized + Serialize,
    {
        let ser = value
            .serialize(CountingSerializer::default())
            .expect("must be able to serialize");
        self.count += ser.count;
        Ok(())
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        Ok(self)
    }
}
impl SerializeSeq for CountingSerializer {
    type Ok = Self;
    type Error = Error;

    fn serialize_element<T>(&mut self, value: &T) -> Result<(), Self::Error>
    where
        T: ?Sized + Serialize,
    {
        let ser = value
            .serialize(CountingSerializer::default())
            .expect("must be able to serialize");
        self.count += ser.count;
        Ok(())
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        Ok(self)
    }
}
impl SerializeTuple for CountingSerializer {
    type Ok = Self;
    type Error = Error;

    fn serialize_element<T>(&mut self, value: &T) -> Result<(), Self::Error>
    where
        T: ?Sized + Serialize,
    {
        let ser = value
            .serialize(CountingSerializer::default())
            .expect("must be able to serialize");
        self.count += ser.count;
        Ok(())
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        Ok(self)
    }
}

impl SerializeStruct for CountingSerializer {
    type Ok = Self;
    type Error = Error;

    fn serialize_field<T>(&mut self, _key: &'static str, value: &T) -> Result<(), Self::Error>
    where
        T: ?Sized + Serialize,
    {
        let ser = value
            .serialize(CountingSerializer::default())
            .expect("must be able to serialize");
        self.count += ser.count;
        Ok(())
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        Ok(self)
    }
}

impl SerializeMap for CountingSerializer {
    type Ok = Self;
    type Error = Error;

    fn serialize_key<T>(&mut self, key: &T) -> Result<(), Self::Error>
    where
        T: ?Sized + Serialize,
    {
        let ser = key
            .serialize(CountingSerializer::default())
            .expect("must be able to serialize");
        self.count += ser.count;
        Ok(())
    }

    fn serialize_value<T>(&mut self, value: &T) -> Result<(), Self::Error>
    where
        T: ?Sized + Serialize,
    {
        let ser = value
            .serialize(CountingSerializer::default())
            .expect("must be able to serialize");
        self.count += ser.count;
        Ok(())
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        Ok(self)
    }
}

impl SerializeTupleVariant for CountingSerializer {
    type Ok = Self;
    type Error = Error;

    fn serialize_field<T>(&mut self, value: &T) -> Result<(), Self::Error>
    where
        T: ?Sized + Serialize,
    {
        let ser = value
            .serialize(CountingSerializer::default())
            .expect("must be able to serialize");
        self.count += ser.count;
        Ok(())
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        Ok(self)
    }
}

impl SerializeTupleStruct for CountingSerializer {
    type Ok = Self;
    type Error = Error;

    fn serialize_field<T>(&mut self, value: &T) -> Result<(), Self::Error>
    where
        T: ?Sized + Serialize,
    {
        let ser = value
            .serialize(CountingSerializer::default())
            .expect("must be able to serialize");
        self.count += ser.count;
        Ok(())
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        Ok(self)
    }
}

#[cfg(test)]
mod test {
    use serde::Serialize;

    use crate::cache::estimate_size;

    #[test]
    fn test_estimate_size() {
        #[derive(Serialize)]
        struct Test {
            string: String,
            u8: u8,
            embedded: TestEmbedded,
        }

        #[derive(Serialize)]
        struct TestEmbedded {
            string: String,
            u8: u8,
        }

        // Baseline
        let s = estimate_size(&Test {
            string: "".to_string(),
            u8: 0,
            embedded: TestEmbedded {
                string: "".to_string(),
                u8: 0,
            },
        });
        assert_eq!(s, 50);

        // Test modifying the root struct
        let s = estimate_size(&Test {
            string: "test".to_string(),
            u8: 0,
            embedded: TestEmbedded {
                string: "".to_string(),
                u8: 0,
            },
        });
        assert_eq!(s, 54);

        // Test modifying the embedded struct
        let s = estimate_size(&Test {
            string: "".to_string(),
            u8: 0,
            embedded: TestEmbedded {
                string: "test".to_string(),
                u8: 0,
            },
        });
        assert_eq!(s, 54);
    }
}
