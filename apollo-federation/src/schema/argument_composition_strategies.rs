use apollo_compiler::ast::Value;
use apollo_compiler::name;
use apollo_compiler::schema::Type;
use lazy_static::lazy_static;

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

lazy_static! {
    pub(crate) static ref MAX_STRATEGY: MaxArgumentCompositionStrategy =
        MaxArgumentCompositionStrategy::new();
    pub(crate) static ref MIN_STRATEGY: MinArgumentCompositionStrategy =
        MinArgumentCompositionStrategy::new();
    pub(crate) static ref SUM_STRATEGY: SumArgumentCompositionStrategy =
        SumArgumentCompositionStrategy::new();
    pub(crate) static ref INTERSECTION_STRATEGY: IntersectionArgumentCompositionStrategy =
        IntersectionArgumentCompositionStrategy {};
    pub(crate) static ref UNION_STRATEGY: UnionArgumentCompositionStrategy =
        UnionArgumentCompositionStrategy {};
}

impl ArgumentCompositionStrategy {
    pub fn name(&self) -> &str {
        match self {
            Self::Max => MAX_STRATEGY.name(),
            Self::Min => MIN_STRATEGY.name(),
            Self::Sum => SUM_STRATEGY.name(),
            Self::Intersection => INTERSECTION_STRATEGY.name(),
            Self::Union => UNION_STRATEGY.name(),
        }
    }

    pub fn is_type_supported(&self, schema: &FederationSchema, ty: &Type) -> Result<(), String> {
        match self {
            Self::Max => MAX_STRATEGY.is_type_supported(schema, ty),
            Self::Min => MIN_STRATEGY.is_type_supported(schema, ty),
            Self::Sum => SUM_STRATEGY.is_type_supported(schema, ty),
            Self::Intersection => INTERSECTION_STRATEGY.is_type_supported(schema, ty),
            Self::Union => UNION_STRATEGY.is_type_supported(schema, ty),
        }
    }

    pub fn merge_values(&self, values: &[Value]) -> Value {
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
    pub supported_types: Vec<Type>,
}

impl FixedTypeSupportValidator {
    fn is_type_supported(&self, _schema: &FederationSchema, ty: &Type) -> Result<(), String> {
        let is_supported = self
            .supported_types
            .iter()
            .any(|supported_ty| *supported_ty == *ty);
        if is_supported {
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

fn int_type() -> Type {
    Type::Named(name!("Int"))
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
                supported_types: vec![int_type().non_null()],
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

    // TODO: check if this neeeds to be an Result<Value> to avoid the panic!()
    // https://apollographql.atlassian.net/browse/FED-170
    fn merge_values(&self, values: &[Value]) -> Value {
        values
            .iter()
            .map(|val| match val {
                Value::Int(i) => i.try_to_i32().unwrap(),
                _ => panic!("Unexpected value type"),
            })
            .max()
            .unwrap_or_default()
            .into()
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
                supported_types: vec![int_type().non_null()],
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

    // TODO: check if this neeeds to be an Result<Value> to avoid the panic!()
    // https://apollographql.atlassian.net/browse/FED-170
    fn merge_values(&self, values: &[Value]) -> Value {
        values
            .iter()
            .map(|val| match val {
                Value::Int(i) => i.try_to_i32().unwrap(),
                _ => panic!("Unexpected value type"),
            })
            .min()
            .unwrap_or_default()
            .into()
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
                supported_types: vec![int_type().non_null()],
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

    // TODO: check if this neeeds to be an Result<Value> to avoid the panic!()
    // https://apollographql.atlassian.net/browse/FED-170
    fn merge_values(&self, values: &[Value]) -> Value {
        values
            .iter()
            .map(|val| match val {
                Value::Int(i) => i.try_to_i32().unwrap(),
                _ => panic!("Unexpected value type"),
            })
            .sum::<i32>()
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

    // TODO: check if this neeeds to be an Result<Value> to avoid the panic!()
    // https://apollographql.atlassian.net/browse/FED-170
    fn merge_values(&self, values: &[Value]) -> Value {
        // Each item in `values` must be a Value::List(...).
        values
            .split_first()
            .map(|(first, rest)| {
                let first_ls = first.as_list().unwrap();
                // Not a super efficient implementation, but we don't expect large problem sizes.
                let mut result = first_ls.to_vec();
                for val in rest {
                    let val_ls = val.as_list().unwrap();
                    result.retain(|result_item| val_ls.iter().any(|n| *n == *result_item));
                }
                Value::List(result)
            })
            .unwrap()
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

    // TODO: check if this neeeds to be an Result<Value> to avoid the panic!()
    // https://apollographql.atlassian.net/browse/FED-170
    fn merge_values(&self, values: &[Value]) -> Value {
        // Each item in `values` must be a Value::List(...).
        // Not a super efficient implementation, but we don't expect large problem sizes.
        let mut result = Vec::new();
        for val in values {
            let val_ls = val.as_list().unwrap();
            for x in val_ls.iter() {
                if !result.contains(x) {
                    result.push(x.clone());
                }
            }
        }
        Value::List(result)
    }
}
