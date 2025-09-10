use crate::types::{Key, MultipleKeys, Value};

macro_rules! tuple2val {
    ($($id:tt $ty:ident);+) => {
impl<$($ty: Into<Value>),+ > From<($($ty),+)> for Value {
    fn from(value: ($($ty),+)) -> Self {
        Value::Array(vec![$(value.$id.into()),+])
    }
}

impl<$($ty: Into<Key>),+ > From<($($ty),+)> for MultipleKeys {
    fn from(value: ($($ty),+)) -> Self {
        Self{keys:vec![$(value.$id.into()),+]}
    }
}
    };
}

tuple2val!(0 A0; 1 A1);
tuple2val!(0 A0; 1 A1; 2 A2);
tuple2val!(0 A0; 1 A1; 2 A2; 3 A3);
tuple2val!(0 A0; 1 A1; 2 A2; 3 A3; 4 A4);
tuple2val!(0 A0; 1 A1; 2 A2; 3 A3; 4 A4; 5 A5);
tuple2val!(0 A0; 1 A1; 2 A2; 3 A3; 4 A4; 5 A5; 6 A6);
tuple2val!(0 A0; 1 A1; 2 A2; 3 A3; 4 A4; 5 A5; 6 A6; 7 A7);
tuple2val!(0 A0; 1 A1; 2 A2; 3 A3; 4 A4; 5 A5; 6 A6; 7 A7; 8 A8);
tuple2val!(0 A0; 1 A1; 2 A2; 3 A3; 4 A4; 5 A5; 6 A6; 7 A7; 8 A8; 9 A9);
tuple2val!(0 A0; 1 A1; 2 A2; 3 A3; 4 A4; 5 A5; 6 A6; 7 A7; 8 A8; 9 A9; 10 A10);
tuple2val!(0 A0; 1 A1; 2 A2; 3 A3; 4 A4; 5 A5; 6 A6; 7 A7; 8 A8; 9 A9; 10 A10; 11 A11);
tuple2val!(0 A0; 1 A1; 2 A2; 3 A3; 4 A4; 5 A5; 6 A6; 7 A7; 8 A8; 9 A9; 10 A10; 11 A11; 12 A12);
tuple2val!(0 A0; 1 A1; 2 A2; 3 A3; 4 A4; 5 A5; 6 A6; 7 A7; 8 A8; 9 A9; 10 A10; 11 A11; 12 A12; 13 A13);
tuple2val!(0 A0; 1 A1; 2 A2; 3 A3; 4 A4; 5 A5; 6 A6; 7 A7; 8 A8; 9 A9; 10 A10; 11 A11; 12 A12; 13 A13; 14 A14);
tuple2val!(0 A0; 1 A1; 2 A2; 3 A3; 4 A4; 5 A5; 6 A6; 7 A7; 8 A8; 9 A9; 10 A10; 11 A11; 12 A12; 13 A13; 14 A14; 15
A15);
tuple2val!(0 A0; 1 A1; 2 A2; 3 A3; 4 A4; 5 A5; 6 A6; 7 A7; 8 A8; 9 A9; 10 A10; 11 A11; 12 A12; 13 A13; 14
A14; 15 A15; 16 A16);
