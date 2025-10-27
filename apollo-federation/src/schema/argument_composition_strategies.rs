use std::sync::LazyLock;

use apollo_compiler::ast::Value;
use apollo_compiler::collections::HashSet;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::schema::Type;
use apollo_compiler::ty;

use crate::schema::FederationSchema;

#[allow(dead_code)]
#[derive(Clone, Copy)]
pub(crate) enum ArgumentCompositionStrategy {
    Max,
    Min,
    // Sum,
    Intersection,
    Union,
    NullableAnd,
    NullableMax,
    NullableUnion,
}

pub(crate) static MAX_STRATEGY: LazyLock<MaxArgumentCompositionStrategy> =
    LazyLock::new(MaxArgumentCompositionStrategy::new);
pub(crate) static MIN_STRATEGY: LazyLock<MinArgumentCompositionStrategy> =
    LazyLock::new(MinArgumentCompositionStrategy::new);
// pub(crate) static SUM_STRATEGY: LazyLock<SumArgumentCompositionStrategy> =
//     LazyLock::new(SumArgumentCompositionStrategy::new);
pub(crate) static INTERSECTION_STRATEGY: LazyLock<IntersectionArgumentCompositionStrategy> =
    LazyLock::new(|| IntersectionArgumentCompositionStrategy {});
pub(crate) static UNION_STRATEGY: LazyLock<UnionArgumentCompositionStrategy> =
    LazyLock::new(|| UnionArgumentCompositionStrategy {});
pub(crate) static NULLABLE_AND_STRATEGY: LazyLock<NullableAndArgumentCompositionStrategy> =
    LazyLock::new(NullableAndArgumentCompositionStrategy::new);
pub(crate) static NULLABLE_MAX_STRATEGY: LazyLock<NullableMaxArgumentCompositionStrategy> =
    LazyLock::new(NullableMaxArgumentCompositionStrategy::new);
pub(crate) static NULLABLE_UNION_STRATEGY: LazyLock<NullableUnionArgumentCompositionStrategy> =
    LazyLock::new(|| NullableUnionArgumentCompositionStrategy {});

impl ArgumentCompositionStrategy {
    fn get_impl(&self) -> &dyn ArgumentComposition {
        match self {
            Self::Max => &*MAX_STRATEGY,
            Self::Min => &*MIN_STRATEGY,
            // Self::Sum => &*SUM_STRATEGY,
            Self::Intersection => &*INTERSECTION_STRATEGY,
            Self::Union => &*UNION_STRATEGY,
            Self::NullableAnd => &*NULLABLE_AND_STRATEGY,
            Self::NullableMax => &*NULLABLE_MAX_STRATEGY,
            Self::NullableUnion => &*NULLABLE_UNION_STRATEGY,
        }
    }

    pub(crate) fn name(&self) -> &str {
        self.get_impl().name()
    }

    pub(crate) fn is_type_supported(
        &self,
        schema: &FederationSchema,
        ty: &Type,
    ) -> Result<(), String> {
        self.get_impl().is_type_supported(schema, ty)
    }

    /// Merges the argument values by the specified strategy.
    /// - `None` return value indicates that the merged value is undefined (meaning the argument
    ///   should be omitted).
    /// - PORT_NOTE: The JS implementation could handle `undefined` input values. However, in Rust,
    ///   undefined values should be omitted in `values`, instead.
    pub(crate) fn merge_values(&self, values: &[Value]) -> Option<Value> {
        self.get_impl().merge_values(values)
    }
}

//////////////////////////////////////////////////////////////////////////////
// Implementation of ArgumentCompositionStrategy's

/// Argument composition strategy for directives.
///
/// Determines how to compose the merged argument value from directive
/// applications across subgraph schemas.
pub(crate) trait ArgumentComposition {
    /// The name of the validator.
    fn name(&self) -> &str;
    /// Is the type `ty`  supported by this validator?
    fn is_type_supported(&self, schema: &FederationSchema, ty: &Type) -> Result<(), String>;
    /// Merges the argument values.
    /// - Assumes that schemas are validated by `is_type_supported`.
    /// - `None` return value indicates that the merged value is undefined (meaning the argument
    ///   should be omitted).
    /// - PORT_NOTE: The JS implementation could handle `undefined` input values. However, in Rust,
    ///   undefined values should be omitted in `values`, instead.
    fn merge_values(&self, values: &[Value]) -> Option<Value>;
}

#[derive(Clone)]
pub(crate) struct FixedTypeSupportValidator {
    pub(crate) supported_types: Vec<Type>,
}

impl FixedTypeSupportValidator {
    fn is_type_supported(&self, _schema: &FederationSchema, ty: &Type) -> Result<(), String> {
        if self.supported_types.contains(ty) {
            return Ok(());
        }

        let expected_types: Vec<String> = self
            .supported_types
            .iter()
            .map(|ty| ty.to_string())
            .collect();
        Err(format!("type(s) {}", expected_types.join(", ")))
    }
}

fn support_any_non_null_array(ty: &Type) -> Result<(), String> {
    if !ty.is_non_null() || !ty.is_list() {
        Err("non-nullable list types of any type".to_string())
    } else {
        Ok(())
    }
}

/// Support for nullable or non-nullable list types of any type.
fn support_any_array(ty: &Type) -> Result<(), String> {
    if ty.is_list() {
        Ok(())
    } else {
        Err("list types of any type".to_string())
    }
}

fn max_int_value<'a>(values: impl Iterator<Item = &'a Value>) -> Value {
    values
        .filter_map(|val| match val {
            // project the value to i32 (GraphQL `Int` scalar type is a signed 32-bit integer)
            Value::Int(i) => i.try_to_i32().ok().map(|i| (val, i)),
            _ => None, // Shouldn't happen
        })
        .max_by(|x, y| x.1.cmp(&y.1))
        .map(|(val, _)| val)
        .cloned()
        // PORT_NOTE: JS uses `Math.max` which returns `-Infinity` for empty values.
        //            Here, we use `i32::MIN`.
        .unwrap_or_else(|| Value::Int(i32::MIN.into()))
}

fn min_int_value<'a>(values: impl Iterator<Item = &'a Value>) -> Value {
    values
        .filter_map(|val| match val {
            // project the value to i32 (GraphQL `Int` scalar type is a signed 32-bit integer)
            Value::Int(i) => i.try_to_i32().ok().map(|i| (val, i)),
            _ => None, // Shouldn't happen
        })
        .min_by(|x, y| x.1.cmp(&y.1))
        .map(|(val, _)| val)
        .cloned()
        // PORT_NOTE: JS uses `Math.min` which returns `+Infinity` for empty values.
        //            Here, we use `i32::MAX` as default value.
        .unwrap_or_else(|| Value::Int(i32::MAX.into()))
}

fn union_list_values<'a>(values: impl Iterator<Item = &'a Value>) -> Value {
    // Each item in `values` must be a Value::List(...).
    // Using `IndexSet` to maintain order and uniqueness.
    // TODO: port `valueEquals` from JS to Rust
    let mut result = IndexSet::default();
    for val in values {
        result.extend(val.as_list().unwrap_or_default().iter().cloned());
    }
    Value::List(result.into_iter().collect())
}

/// Merge nullable values.
/// - Returns `None` if all values are `null` or values are empty.
/// - Otherwise, calls `merge_values` argument with all the non-null values and returns the result.
// NOTE: This function makes the assumption that for the directive argument
// being merged, it is not "nullable with non-null default" in the supergraph
// schema (this kind of type/default combo is confusing and should be avoided,
// if possible). This assumption allows this function to replace null with
// None, which makes for a cleaner supergraph schema.
fn merge_nullable_values(
    values: &[Value],
    merge_values: impl Fn(&[&Value]) -> Value,
) -> Option<Value> {
    // Filter out `null` values.
    let values = values.iter().filter(|v| !v.is_null()).collect::<Vec<_>>();
    if values.is_empty() {
        return None; // No values to merge, return None (instead of null)
    }
    merge_values(&values).into()
}

// MAX
#[derive(Clone)]
pub(crate) struct MaxArgumentCompositionStrategy {
    validator: FixedTypeSupportValidator,
}

impl MaxArgumentCompositionStrategy {
    fn new() -> Self {
        Self {
            validator: FixedTypeSupportValidator {
                supported_types: vec![ty!(Int!)],
            },
        }
    }
}

impl ArgumentComposition for MaxArgumentCompositionStrategy {
    fn name(&self) -> &str {
        "MAX"
    }

    fn is_type_supported(&self, schema: &FederationSchema, ty: &Type) -> Result<(), String> {
        self.validator.is_type_supported(schema, ty)
    }

    fn merge_values(&self, values: &[Value]) -> Option<Value> {
        max_int_value(values.iter()).into()
    }
}

// MIN
#[derive(Clone)]
pub(crate) struct MinArgumentCompositionStrategy {
    validator: FixedTypeSupportValidator,
}

impl MinArgumentCompositionStrategy {
    fn new() -> Self {
        Self {
            validator: FixedTypeSupportValidator {
                supported_types: vec![ty!(Int!)],
            },
        }
    }
}

impl ArgumentComposition for MinArgumentCompositionStrategy {
    fn name(&self) -> &str {
        "MIN"
    }

    fn is_type_supported(&self, schema: &FederationSchema, ty: &Type) -> Result<(), String> {
        self.validator.is_type_supported(schema, ty)
    }

    fn merge_values(&self, values: &[Value]) -> Option<Value> {
        min_int_value(values.iter()).into()
    }
}

// NOTE: This doesn't work today because directive applications are de-duped
// before being merged, we'd need to modify merge logic if we need this kind
// of behavior.
// SUM
// #[derive(Clone)]
// pub(crate) struct SumArgumentCompositionStrategy {
//     validator: FixedTypeSupportValidator,
// }

// impl SumArgumentCompositionStrategy {
//     fn new() -> Self {
//         Self {
//             validator: FixedTypeSupportValidator {
//                 supported_types: vec![ty!(Int!)],
//             },
//         }
//     }
// }

// impl ArgumentComposition for SumArgumentCompositionStrategy {
//     fn name(&self) -> &str {
//         "SUM"
//     }

//     fn is_type_supported(&self, schema: &FederationSchema, ty: &Type) -> Result<(), String> {
//         self.validator.is_type_supported(schema, ty)
//     }

//     fn merge_values(&self, values: &[Value]) -> Value {
//         values
//             .iter()
//             .map(|val| match val {
//                 // project the value to i32 (GraphQL `Int` scalar type is a signed 32-bit integer)
//                 Value::Int(i) => i.try_to_i32().unwrap_or(0),
//                 _ => 0, // Shouldn't happen
//             })
//             // Note: `.sum()` would panic if the sum overflows.
//             .fold(0_i32, |acc, i| acc.saturating_add(i))
//             .into()
//     }
// }

// INTERSECTION
#[derive(Clone)]
pub(crate) struct IntersectionArgumentCompositionStrategy {}

impl ArgumentComposition for IntersectionArgumentCompositionStrategy {
    fn name(&self) -> &str {
        "INTERSECTION"
    }

    fn is_type_supported(&self, _schema: &FederationSchema, ty: &Type) -> Result<(), String> {
        support_any_non_null_array(ty)
    }

    fn merge_values(&self, values: &[Value]) -> Option<Value> {
        // Each item in `values` must be a Value::List(...).
        let Some((first, rest)) = values.split_first() else {
            return Value::List(Vec::new()).into(); // Fallback for empty list
        };

        let mut result = first.as_list().unwrap_or_default().to_owned();
        for val in rest {
            // TODO: port `valueEquals` from JS to Rust
            let val_set: HashSet<&Value> = val
                .as_list()
                .unwrap_or_default()
                .iter()
                .map(|v| v.as_ref())
                .collect();
            result.retain(|result_item| val_set.contains(result_item.as_ref()));
        }
        Value::List(result).into()
    }
}

// UNION
#[derive(Clone)]
pub(crate) struct UnionArgumentCompositionStrategy {}

impl ArgumentComposition for UnionArgumentCompositionStrategy {
    fn name(&self) -> &str {
        "UNION"
    }

    fn is_type_supported(&self, _schema: &FederationSchema, ty: &Type) -> Result<(), String> {
        support_any_non_null_array(ty)
    }

    fn merge_values(&self, values: &[Value]) -> Option<Value> {
        union_list_values(values.iter()).into()
    }
}

// NULLABLE_AND
#[derive(Clone)]
pub(crate) struct NullableAndArgumentCompositionStrategy {
    type_validator: FixedTypeSupportValidator,
}

impl NullableAndArgumentCompositionStrategy {
    fn new() -> Self {
        Self {
            type_validator: FixedTypeSupportValidator {
                supported_types: vec![ty!(Boolean), ty!(Boolean!)],
            },
        }
    }
}

impl ArgumentComposition for NullableAndArgumentCompositionStrategy {
    fn name(&self) -> &str {
        "NULLABLE_AND"
    }

    fn is_type_supported(&self, schema: &FederationSchema, ty: &Type) -> Result<(), String> {
        self.type_validator.is_type_supported(schema, ty)
    }

    fn merge_values(&self, values: &[Value]) -> Option<Value> {
        merge_nullable_values(values, |values| {
            values
                .iter()
                .all(|v| {
                    if let Value::Boolean(b) = v {
                        *b
                    } else {
                        false // shouldn't happen
                    }
                })
                .into()
        })
    }
}

// NULLABLE_MAX
#[derive(Clone)]
pub(crate) struct NullableMaxArgumentCompositionStrategy {
    type_validator: FixedTypeSupportValidator,
}

impl NullableMaxArgumentCompositionStrategy {
    fn new() -> Self {
        Self {
            type_validator: FixedTypeSupportValidator {
                supported_types: vec![ty!(Int), ty!(Int!)],
            },
        }
    }
}

impl ArgumentComposition for NullableMaxArgumentCompositionStrategy {
    fn name(&self) -> &str {
        "NULLABLE_MAX"
    }

    fn is_type_supported(&self, schema: &FederationSchema, ty: &Type) -> Result<(), String> {
        self.type_validator.is_type_supported(schema, ty)
    }

    fn merge_values(&self, values: &[Value]) -> Option<Value> {
        merge_nullable_values(values, |values| max_int_value(values.iter().copied()))
    }
}

// NULLABLE_UNION
#[derive(Clone)]
pub(crate) struct NullableUnionArgumentCompositionStrategy {}

impl ArgumentComposition for NullableUnionArgumentCompositionStrategy {
    fn name(&self) -> &str {
        "NULLABLE_UNION"
    }

    fn is_type_supported(&self, _schema: &FederationSchema, ty: &Type) -> Result<(), String> {
        support_any_array(ty)
    }

    fn merge_values(&self, values: &[Value]) -> Option<Value> {
        merge_nullable_values(values, |values| union_list_values(values.iter().copied()))
    }
}
