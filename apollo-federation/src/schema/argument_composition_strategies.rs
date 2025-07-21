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
    Sum,
    Intersection,
    Union,
}

pub(crate) static MAX_STRATEGY: LazyLock<MaxArgumentCompositionStrategy> =
    LazyLock::new(MaxArgumentCompositionStrategy::new);
pub(crate) static MIN_STRATEGY: LazyLock<MinArgumentCompositionStrategy> =
    LazyLock::new(MinArgumentCompositionStrategy::new);
pub(crate) static SUM_STRATEGY: LazyLock<SumArgumentCompositionStrategy> =
    LazyLock::new(SumArgumentCompositionStrategy::new);
pub(crate) static INTERSECTION_STRATEGY: LazyLock<IntersectionArgumentCompositionStrategy> =
    LazyLock::new(|| IntersectionArgumentCompositionStrategy {});
pub(crate) static UNION_STRATEGY: LazyLock<UnionArgumentCompositionStrategy> =
    LazyLock::new(|| UnionArgumentCompositionStrategy {});

impl ArgumentCompositionStrategy {
    pub(crate) fn name(&self) -> &str {
        match self {
            Self::Max => MAX_STRATEGY.name(),
            Self::Min => MIN_STRATEGY.name(),
            Self::Sum => SUM_STRATEGY.name(),
            Self::Intersection => INTERSECTION_STRATEGY.name(),
            Self::Union => UNION_STRATEGY.name(),
        }
    }

    pub(crate) fn is_type_supported(
        &self,
        schema: &FederationSchema,
        ty: &Type,
    ) -> Result<(), String> {
        match self {
            Self::Max => MAX_STRATEGY.is_type_supported(schema, ty),
            Self::Min => MIN_STRATEGY.is_type_supported(schema, ty),
            Self::Sum => SUM_STRATEGY.is_type_supported(schema, ty),
            Self::Intersection => INTERSECTION_STRATEGY.is_type_supported(schema, ty),
            Self::Union => UNION_STRATEGY.is_type_supported(schema, ty),
        }
    }

    pub(crate) fn merge_values(&self, values: &[Value]) -> Value {
        match self {
            Self::Max => MAX_STRATEGY.merge_values(values),
            Self::Min => MIN_STRATEGY.merge_values(values),
            Self::Sum => SUM_STRATEGY.merge_values(values),
            Self::Intersection => INTERSECTION_STRATEGY.merge_values(values),
            Self::Union => UNION_STRATEGY.merge_values(values),
        }
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
    /// Assumes that schemas are validated by `is_type_supported`.
    fn merge_values(&self, values: &[Value]) -> Value;
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

    fn merge_values(&self, values: &[Value]) -> Value {
        values
            .iter()
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

    fn merge_values(&self, values: &[Value]) -> Value {
        values
            .iter()
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
}

// SUM
#[derive(Clone)]
pub(crate) struct SumArgumentCompositionStrategy {
    validator: FixedTypeSupportValidator,
}

impl SumArgumentCompositionStrategy {
    fn new() -> Self {
        Self {
            validator: FixedTypeSupportValidator {
                supported_types: vec![ty!(Int!)],
            },
        }
    }
}

impl ArgumentComposition for SumArgumentCompositionStrategy {
    fn name(&self) -> &str {
        "SUM"
    }

    fn is_type_supported(&self, schema: &FederationSchema, ty: &Type) -> Result<(), String> {
        self.validator.is_type_supported(schema, ty)
    }

    fn merge_values(&self, values: &[Value]) -> Value {
        values
            .iter()
            .map(|val| match val {
                // project the value to i32 (GraphQL `Int` scalar type is a signed 32-bit integer)
                Value::Int(i) => i.try_to_i32().unwrap_or(0),
                _ => 0, // Shouldn't happen
            })
            // Note: `.sum()` would panic if the sum overflows.
            .fold(0_i32, |acc, i| acc.saturating_add(i))
            .into()
    }
}

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

    fn merge_values(&self, values: &[Value]) -> Value {
        // Each item in `values` must be a Value::List(...).
        let Some((first, rest)) = values.split_first() else {
            return Value::List(Vec::new()); // Fallback for empty list
        };

        let mut result = first.as_list().unwrap_or_default().to_owned();
        for val in rest {
            let val_set: HashSet<&Value> = val
                .as_list()
                .unwrap_or_default()
                .iter()
                .map(|v| v.as_ref())
                .collect();
            result.retain(|result_item| val_set.contains(result_item.as_ref()));
        }
        Value::List(result)
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

    fn merge_values(&self, values: &[Value]) -> Value {
        // Each item in `values` must be a Value::List(...).
        // Using `IndexSet` to maintain order and uniqueness.
        let mut result = IndexSet::default();
        for val in values {
            result.extend(val.as_list().unwrap_or_default().iter().cloned());
        }
        Value::List(result.into_iter().collect())
    }
}
