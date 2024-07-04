use std::cell::RefCell;
use std::hash::BuildHasherDefault;
use std::hash::Hash;

use indexmap::set::IndexSet;
use opentelemetry::StringValue;
use opentelemetry::Value;
use rmp::encode::RmpWrite;
use rmp::encode::ValueWriteError;

type InternHasher = ahash::AHasher;

#[derive(PartialEq)]
pub(crate) enum InternValue<'a> {
    RegularString(&'a str),
    OpenTelemetryValue(&'a Value),
}

impl<'a> Hash for InternValue<'a> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        match &self {
            InternValue::RegularString(s) => s.hash(state),
            InternValue::OpenTelemetryValue(v) => match v {
                Value::Bool(x) => x.hash(state),
                Value::I64(x) => x.hash(state),
                Value::String(x) => x.hash(state),
                Value::F64(x) => x.to_bits().hash(state),
                Value::Array(a) => match a {
                    opentelemetry::Array::Bool(x) => x.hash(state),
                    opentelemetry::Array::I64(x) => x.hash(state),
                    opentelemetry::Array::F64(floats) => {
                        for f in floats {
                            f.to_bits().hash(state);
                        }
                    }
                    opentelemetry::Array::String(x) => x.hash(state),
                },
            },
        }
    }
}

impl<'a> Eq for InternValue<'a> {}

const BOOLEAN_TRUE: &str = "true";
const BOOLEAN_FALSE: &str = "false";
const LEFT_SQUARE_BRACKET: u8 = b'[';
const RIGHT_SQUARE_BRACKET: u8 = b']';
const COMMA: u8 = b',';
const DOUBLE_QUOTE: u8 = b'"';
const EMPTY_ARRAY: &str = "[]";

trait WriteAsLiteral {
    fn write_to(&self, buffer: &mut Vec<u8>);
}

impl WriteAsLiteral for bool {
    fn write_to(&self, buffer: &mut Vec<u8>) {
        buffer.extend_from_slice(if *self { BOOLEAN_TRUE } else { BOOLEAN_FALSE }.as_bytes());
    }
}

impl WriteAsLiteral for i64 {
    fn write_to(&self, buffer: &mut Vec<u8>) {
        buffer.extend_from_slice(itoa::Buffer::new().format(*self).as_bytes());
    }
}

impl WriteAsLiteral for f64 {
    fn write_to(&self, buffer: &mut Vec<u8>) {
        buffer.extend_from_slice(ryu::Buffer::new().format(*self).as_bytes());
    }
}

impl WriteAsLiteral for StringValue {
    fn write_to(&self, buffer: &mut Vec<u8>) {
        buffer.push(DOUBLE_QUOTE);
        buffer.extend_from_slice(self.as_str().as_bytes());
        buffer.push(DOUBLE_QUOTE);
    }
}

impl<'a> InternValue<'a> {
    pub(crate) fn write_as_str<W: RmpWrite>(
        &self,
        payload: &mut W,
        reusable_buffer: &mut Vec<u8>,
    ) -> Result<(), ValueWriteError<W::Error>> {
        match self {
            InternValue::RegularString(x) => rmp::encode::write_str(payload, x),
            InternValue::OpenTelemetryValue(v) => match v {
                Value::Bool(x) => {
                    rmp::encode::write_str(payload, if *x { BOOLEAN_TRUE } else { BOOLEAN_FALSE })
                }
                Value::I64(x) => rmp::encode::write_str(payload, itoa::Buffer::new().format(*x)),
                Value::F64(x) => rmp::encode::write_str(payload, ryu::Buffer::new().format(*x)),
                Value::String(x) => rmp::encode::write_str(payload, x.as_ref()),
                Value::Array(array) => match array {
                    opentelemetry::Array::Bool(x) => {
                        Self::write_generic_array(payload, reusable_buffer, x)
                    }
                    opentelemetry::Array::I64(x) => {
                        Self::write_generic_array(payload, reusable_buffer, x)
                    }
                    opentelemetry::Array::F64(x) => {
                        Self::write_generic_array(payload, reusable_buffer, x)
                    }
                    opentelemetry::Array::String(x) => {
                        Self::write_generic_array(payload, reusable_buffer, x)
                    }
                },
            },
        }
    }

    fn write_empty_array<W: RmpWrite>(payload: &mut W) -> Result<(), ValueWriteError<W::Error>> {
        rmp::encode::write_str(payload, EMPTY_ARRAY)
    }

    fn write_buffer_as_string<W: RmpWrite>(
        payload: &mut W,
        reusable_buffer: &[u8],
    ) -> Result<(), ValueWriteError<W::Error>> {
        rmp::encode::write_str_len(payload, reusable_buffer.len() as u32)?;
        payload
            .write_bytes(reusable_buffer)
            .map_err(ValueWriteError::InvalidDataWrite)
    }

    fn write_generic_array<W: RmpWrite, T: WriteAsLiteral>(
        payload: &mut W,
        reusable_buffer: &mut Vec<u8>,
        array: &[T],
    ) -> Result<(), ValueWriteError<W::Error>> {
        if array.is_empty() {
            return Self::write_empty_array(payload);
        }

        reusable_buffer.clear();
        reusable_buffer.push(LEFT_SQUARE_BRACKET);

        array[0].write_to(reusable_buffer);

        for value in array[1..].iter() {
            reusable_buffer.push(COMMA);
            value.write_to(reusable_buffer);
        }

        reusable_buffer.push(RIGHT_SQUARE_BRACKET);

        Self::write_buffer_as_string(payload, reusable_buffer)
    }
}

pub(crate) struct StringInterner<'a> {
    data: IndexSet<InternValue<'a>, BuildHasherDefault<InternHasher>>,
}

impl<'a> StringInterner<'a> {
    pub(crate) fn new() -> StringInterner<'a> {
        StringInterner {
            data: IndexSet::with_capacity_and_hasher(128, BuildHasherDefault::default()),
        }
    }

    pub(crate) fn intern(&mut self, data: &'a str) -> u32 {
        if let Some(idx) = self.data.get_index_of(&InternValue::RegularString(data)) {
            return idx as u32;
        }
        self.data.insert_full(InternValue::RegularString(data)).0 as u32
    }

    pub(crate) fn intern_value(&mut self, data: &'a Value) -> u32 {
        if let Some(idx) = self
            .data
            .get_index_of(&InternValue::OpenTelemetryValue(data))
        {
            return idx as u32;
        }
        self.data
            .insert_full(InternValue::OpenTelemetryValue(data))
            .0 as u32
    }

    pub(crate) fn write_dictionary<W: RmpWrite>(
        &self,
        payload: &mut W,
    ) -> Result<(), ValueWriteError<W::Error>> {
        thread_local! {
            static BUFFER: RefCell<Vec<u8>> = RefCell::new(Vec::with_capacity(4096));
        }

        BUFFER.with(|cell| {
            let reusable_buffer = &mut cell.borrow_mut();
            rmp::encode::write_array_len(payload, self.data.len() as u32)?;
            for data in self.data.iter() {
                data.write_as_str(payload, reusable_buffer)?;
            }

            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use opentelemetry::Array;

    use super::*;

    #[test]
    fn test_intern() {
        let a = "a".to_string();
        let b = "b";
        let c = "c";

        let mut intern = StringInterner::new();
        let a_idx = intern.intern(a.as_str());
        let b_idx = intern.intern(b);
        let c_idx = intern.intern(c);
        let d_idx = intern.intern(a.as_str());
        let e_idx = intern.intern(c);

        assert_eq!(a_idx, 0);
        assert_eq!(b_idx, 1);
        assert_eq!(c_idx, 2);
        assert_eq!(d_idx, a_idx);
        assert_eq!(e_idx, c_idx);
    }

    #[test]
    fn test_intern_bool() {
        let a = Value::Bool(true);
        let b = Value::Bool(false);
        let c = "c";

        let mut intern = StringInterner::new();
        let a_idx = intern.intern_value(&a);
        let b_idx = intern.intern_value(&b);
        let c_idx = intern.intern(c);
        let d_idx = intern.intern_value(&a);
        let e_idx = intern.intern(c);

        assert_eq!(a_idx, 0);
        assert_eq!(b_idx, 1);
        assert_eq!(c_idx, 2);
        assert_eq!(d_idx, a_idx);
        assert_eq!(e_idx, c_idx);
    }

    #[test]
    fn test_intern_i64() {
        let a = Value::I64(1234567890);
        let b = Value::I64(-1234567890);
        let c = "c";
        let d = Value::I64(1234567890);

        let mut intern = StringInterner::new();
        let a_idx = intern.intern_value(&a);
        let b_idx = intern.intern_value(&b);
        let c_idx = intern.intern(c);
        let d_idx = intern.intern_value(&a);
        let e_idx = intern.intern(c);
        let f_idx = intern.intern_value(&d);

        assert_eq!(a_idx, 0);
        assert_eq!(b_idx, 1);
        assert_eq!(c_idx, 2);
        assert_eq!(d_idx, a_idx);
        assert_eq!(e_idx, c_idx);
        assert_eq!(f_idx, a_idx);
    }

    #[test]
    fn test_intern_f64() {
        let a = Value::F64(123456.7890);
        let b = Value::F64(-1234567.890);
        let c = "c";
        let d = Value::F64(-1234567.890);

        let mut intern = StringInterner::new();
        let a_idx = intern.intern_value(&a);
        let b_idx = intern.intern_value(&b);
        let c_idx = intern.intern(c);
        let d_idx = intern.intern_value(&a);
        let e_idx = intern.intern(c);
        let f_idx = intern.intern_value(&d);

        assert_eq!(a_idx, 0);
        assert_eq!(b_idx, 1);
        assert_eq!(c_idx, 2);
        assert_eq!(d_idx, a_idx);
        assert_eq!(e_idx, c_idx);
        assert_eq!(b_idx, f_idx);
    }

    #[test]
    fn test_intern_array_of_booleans() {
        let a = Value::Array(Array::Bool(vec![true, false]));
        let b = Value::Array(Array::Bool(vec![false, true]));
        let c = "c";
        let d = Value::Array(Array::Bool(vec![]));
        let f = Value::Array(Array::Bool(vec![false, true]));

        let mut intern = StringInterner::new();
        let a_idx = intern.intern_value(&a);
        let b_idx = intern.intern_value(&b);
        let c_idx = intern.intern(c);
        let d_idx = intern.intern_value(&a);
        let e_idx = intern.intern(c);
        let f_idx = intern.intern_value(&d);
        let g_idx = intern.intern_value(&f);

        assert_eq!(a_idx, 0);
        assert_eq!(b_idx, 1);
        assert_eq!(c_idx, 2);
        assert_eq!(d_idx, a_idx);
        assert_eq!(e_idx, c_idx);
        assert_eq!(f_idx, 3);
        assert_eq!(g_idx, b_idx);
    }

    #[test]
    fn test_intern_array_of_i64() {
        let a = Value::Array(Array::I64(vec![123, -123]));
        let b = Value::Array(Array::I64(vec![-123, 123]));
        let c = "c";
        let d = Value::Array(Array::I64(vec![]));
        let f = Value::Array(Array::I64(vec![-123, 123]));

        let mut intern = StringInterner::new();
        let a_idx = intern.intern_value(&a);
        let b_idx = intern.intern_value(&b);
        let c_idx = intern.intern(c);
        let d_idx = intern.intern_value(&a);
        let e_idx = intern.intern(c);
        let f_idx = intern.intern_value(&d);
        let g_idx = intern.intern_value(&f);

        assert_eq!(a_idx, 0);
        assert_eq!(b_idx, 1);
        assert_eq!(c_idx, 2);
        assert_eq!(d_idx, a_idx);
        assert_eq!(e_idx, c_idx);
        assert_eq!(f_idx, 3);
        assert_eq!(g_idx, b_idx);
    }

    #[test]
    fn test_intern_array_of_f64() {
        let f1 = 123.0f64;
        let f2 = 0f64;

        let a = Value::Array(Array::F64(vec![f1, f2]));
        let b = Value::Array(Array::F64(vec![f2, f1]));
        let c = "c";
        let d = Value::Array(Array::F64(vec![]));
        let f = Value::Array(Array::F64(vec![f2, f1]));

        let mut intern = StringInterner::new();
        let a_idx = intern.intern_value(&a);
        let b_idx = intern.intern_value(&b);
        let c_idx = intern.intern(c);
        let d_idx = intern.intern_value(&a);
        let e_idx = intern.intern(c);
        let f_idx = intern.intern_value(&d);
        let g_idx = intern.intern_value(&f);

        assert_eq!(a_idx, 0);
        assert_eq!(b_idx, 1);
        assert_eq!(c_idx, 2);
        assert_eq!(d_idx, a_idx);
        assert_eq!(e_idx, c_idx);
        assert_eq!(f_idx, 3);
        assert_eq!(g_idx, b_idx);
    }

    #[test]
    fn test_intern_array_of_string() {
        let s1 = "a";
        let s2 = "b";

        let a = Value::Array(Array::String(vec![
            StringValue::from(s1),
            StringValue::from(s2),
        ]));
        let b = Value::Array(Array::String(vec![
            StringValue::from(s2),
            StringValue::from(s1),
        ]));
        let c = "c";
        let d = Value::Array(Array::String(vec![]));
        let f = Value::Array(Array::String(vec![
            StringValue::from(s2),
            StringValue::from(s1),
        ]));

        let mut intern = StringInterner::new();
        let a_idx = intern.intern_value(&a);
        let b_idx = intern.intern_value(&b);
        let c_idx = intern.intern(c);
        let d_idx = intern.intern_value(&a);
        let e_idx = intern.intern(c);
        let f_idx = intern.intern_value(&d);
        let g_idx = intern.intern_value(&f);

        assert_eq!(a_idx, 0);
        assert_eq!(b_idx, 1);
        assert_eq!(c_idx, 2);
        assert_eq!(d_idx, a_idx);
        assert_eq!(e_idx, c_idx);
        assert_eq!(f_idx, 3);
        assert_eq!(g_idx, b_idx);
    }

    #[test]
    fn test_write_boolean_literal() {
        let mut buffer: Vec<u8> = vec![];

        true.write_to(&mut buffer);

        assert_eq!(&buffer[..], b"true");

        buffer.clear();

        false.write_to(&mut buffer);

        assert_eq!(&buffer[..], b"false");
    }

    #[test]
    fn test_write_i64_literal() {
        let mut buffer: Vec<u8> = vec![];

        1234567890i64.write_to(&mut buffer);

        assert_eq!(&buffer[..], b"1234567890");

        buffer.clear();

        (-1234567890i64).write_to(&mut buffer);

        assert_eq!(&buffer[..], b"-1234567890");
    }

    #[test]
    fn test_write_f64_literal() {
        let mut buffer: Vec<u8> = vec![];

        let f1 = 12345.678f64;
        let f2 = -12345.678f64;

        f1.write_to(&mut buffer);

        assert_eq!(&buffer[..], format!("{}", f1).as_bytes());

        buffer.clear();

        f2.write_to(&mut buffer);

        assert_eq!(&buffer[..], format!("{}", f2).as_bytes());
    }

    #[test]
    fn test_write_string_literal() {
        let mut buffer: Vec<u8> = vec![];

        let s1 = StringValue::from("abc");
        let s2 = StringValue::from("");

        s1.write_to(&mut buffer);

        assert_eq!(&buffer[..], format!("\"{}\"", s1).as_bytes());

        buffer.clear();

        s2.write_to(&mut buffer);

        assert_eq!(&buffer[..], format!("\"{}\"", s2).as_bytes());
    }

    fn test_encoding_intern_value(value: InternValue<'_>) {
        let mut expected: Vec<u8> = vec![];
        let mut actual: Vec<u8> = vec![];

        let mut buffer = vec![];

        value.write_as_str(&mut actual, &mut buffer).unwrap();

        let InternValue::OpenTelemetryValue(value) = value else {
            return;
        };

        rmp::encode::write_str(&mut expected, value.as_str().as_ref()).unwrap();

        assert_eq!(expected, actual);
    }

    #[test]
    fn test_encode_boolean() {
        test_encoding_intern_value(InternValue::OpenTelemetryValue(&Value::Bool(true)));
        test_encoding_intern_value(InternValue::OpenTelemetryValue(&Value::Bool(false)));
    }

    #[test]
    fn test_encode_i64() {
        test_encoding_intern_value(InternValue::OpenTelemetryValue(&Value::I64(123)));
        test_encoding_intern_value(InternValue::OpenTelemetryValue(&Value::I64(0)));
        test_encoding_intern_value(InternValue::OpenTelemetryValue(&Value::I64(-123)));
    }

    #[test]
    fn test_encode_f64() {
        test_encoding_intern_value(InternValue::OpenTelemetryValue(&Value::F64(123.456f64)));
        test_encoding_intern_value(InternValue::OpenTelemetryValue(&Value::F64(-123.456f64)));
    }
}
