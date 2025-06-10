use std::fmt::Display;

use crate::codegen::grpc::GrpcCodegenError;

#[derive(Debug)]
#[repr(i32)]
pub enum GrpcType {
    Double = 1,
    Float = 2,
    Int64 = 3,
    Uint64 = 4,
    Int32 = 5,
    Fixed64 = 6,
    Fixed32 = 7,
    Bool = 8,
    String = 9,
    Group = 10,
    Message = 11,
    Bytes = 12,
    Uint32 = 13,
    Enum = 14,
    Sfixed32 = 15,
    Sfixed64 = 16,
    Sint32 = 17,
    Sint64 = 18,
}

impl TryFrom<i32> for GrpcType {
    type Error = GrpcCodegenError;
    fn try_from(value: i32) -> Result<Self, Self::Error> {
        if !(1..=18).contains(&value) {
            Err(GrpcCodegenError::UnknownGrpcType)
        } else {
            // SAFTEY: Borders checked in if branch
            // SAFETY 2: This value should only be created by the GRPC protox, so it should be contained
            Ok(unsafe { std::mem::transmute::<i32, GrpcType>(value) })
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphqlType {
    Int,
    Float,
    String,
    Boolean,
    ID,
    Enum,
    CustomType,
}

impl Display for GraphqlType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl TryFrom<GrpcType> for GraphqlType {
    type Error = GrpcCodegenError;

    fn try_from(value: GrpcType) -> Result<Self, Self::Error> {
        Ok(match value {
            GrpcType::Double => {
                return Err(GrpcCodegenError::TypeNotSupported(
                    GrpcType::Double,
                    GraphqlType::Float,
                ));
            }
            GrpcType::Float => GraphqlType::Float,
            GrpcType::Int64 => GraphqlType::String, // formats like date
            GrpcType::Uint64 => {
                return Err(GrpcCodegenError::TypeNotSupported(
                    GrpcType::Uint64,
                    GraphqlType::Int,
                ));
            }
            GrpcType::Fixed64 => {
                return Err(GrpcCodegenError::TypeNotSupported(
                    GrpcType::Fixed64,
                    GraphqlType::Int,
                ));
            }
            GrpcType::Fixed32 => {
                return Err(GrpcCodegenError::TypeNotSupported(
                    GrpcType::Fixed32,
                    GraphqlType::Int,
                ));
            }
            GrpcType::Bool => GraphqlType::Boolean,
            GrpcType::String => GraphqlType::String,
            GrpcType::Group => GraphqlType::CustomType,
            GrpcType::Message => GraphqlType::CustomType,
            GrpcType::Bytes => GraphqlType::ID,
            GrpcType::Uint32 => {
                return Err(GrpcCodegenError::TypeNotSupported(
                    GrpcType::Uint32,
                    GraphqlType::Int,
                ));
            }
            GrpcType::Enum => GraphqlType::Enum,
            GrpcType::Sfixed64 => {
                return Err(GrpcCodegenError::TypeNotSupported(
                    GrpcType::Sfixed64,
                    GraphqlType::Int,
                ));
            }
            GrpcType::Sint32 | GrpcType::Sfixed32 | GrpcType::Int32 => GraphqlType::Int,
            GrpcType::Sint64 => {
                return Err(GrpcCodegenError::TypeNotSupported(
                    GrpcType::Sint64,
                    GraphqlType::Int,
                ));
            }
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvailableType {
    pub name: String,
    pub graph_name: String,
    pub is_empty: bool,
}
