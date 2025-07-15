// this is temporary as we are porting the type merging logic from the TypeScript implementation
// we will remove this once we have integrated it to the main codebase
#![allow(dead_code)]
#![allow(unused_variables)]

use std::collections::HashMap;

use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::ast::InputValueDefinition;
use apollo_compiler::ast::NamedType;
use apollo_compiler::ast::Type;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::Schema;
use apollo_compiler::schema::Type as GqlType;

use crate::error::CompositionError;
use crate::error::FederationError;
use crate::error::SingleFederationError;
use crate::merger::EnumTypeUsage;
use crate::merger::error_reporter::ErrorReporter;
use crate::merger::hints::HintCode;
use crate::merger::merge::Sources;
use crate::supergraph::CompositionHint;

pub(crate) trait SchemaElementWithType {
    fn coordinate(&self) -> &str;
    fn get_type(&self) -> Option<&Type>;
    fn set_type(&mut self, typ: Type);
}

impl SchemaElementWithType for FieldDefinition {
    fn coordinate(&self) -> &str {
        // could also include parent type name if you have it; for now:
        self.name.as_str()
    }
    fn get_type(&self) -> Option<&GqlType> {
        Some(&self.ty)
    }
    fn set_type(&mut self, typ: GqlType) {
        self.ty = typ;
    }
}

impl SchemaElementWithType for InputValueDefinition {
    fn coordinate(&self) -> &str {
        self.name.as_str()
    }
    fn get_type(&self) -> Option<&GqlType> {
        Some(&self.ty)
    }
    fn set_type(&mut self, typ: GqlType) {
        self.ty = typ.into();
    }
}

/// Type alias for tracking sources of a type across different subgraphs
/// Using the existing Sources<T> pattern from the merger module
pub(crate) type TypeSources = Sources<Type>;

/// Helper functions for working with TypeSources
pub(crate) mod type_sources_helpers {
    use super::*;

    /// Create a new TypeSources with the given number of subgraphs
    pub(crate) fn new(subgraph_count: usize) -> TypeSources {
        let mut sources = IndexMap::default();
        for i in 0..subgraph_count {
            sources.insert(i, None);
        }
        sources
    }

    /// Set the source type for a specific subgraph index
    pub(crate) fn set_source(sources: &mut TypeSources, index: usize, typ: Type) {
        sources.insert(index, Some(typ));
    }

    /// Get iterator over (index, Option<&Type>) pairs
    pub(crate) fn iter_types(
        sources: &TypeSources,
    ) -> impl Iterator<Item = (usize, Option<&Type>)> {
        sources.iter().map(|(idx, typ)| (*idx, typ.as_ref()))
    }
}

/// Subtyping rules for type merging
///
/// Reference: federation/internals-js/src/types.ts:20-30 (ALL_SUBTYPING_RULES)
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SubtypingRule {
    /// Direct subtyping (interface/union relationships)
    Direct,
    /// Non-nullable downgrade (NonNull T is subtype of T)
    NonNullableDowngrade,
    /// List upgrade (T is subtype of [T])
    ListUpgrade,
    /// List propagation (List T is subtype of List U if T is subtype of U)
    ListPropagation,
    /// Non-nullable propagation (NonNull T is subtype of NonNull U if T is subtype of U)
    NonNullablePropagation,
}

/// Advanced subtyping rules configuration - ported from TypeScript composition options
/// Controls which subtyping relationships are allowed during type merging
#[derive(Debug, Clone)]
pub(crate) struct SubtypingRules {
    /// Allowed subtyping rules
    pub(crate) allowed_rules: Vec<SubtypingRule>,
}

impl Default for SubtypingRules {
    fn default() -> Self {
        // Default subtyping rules
        Self {
            allowed_rules: vec![
                SubtypingRule::Direct,
                SubtypingRule::NonNullableDowngrade,
                SubtypingRule::ListPropagation,
                SubtypingRule::NonNullablePropagation,
                // Note: ListUpgrade is excluded by default, matching TypeScript behavior
            ],
        }
    }
}

/// Main type merger that handles the logic of merging types across subgraphs
/// Advanced type merger with error reporting and hint generation
/// Enhanced version of TypeScript Merger class focused on type merging
pub(crate) struct TypeMerger<'a> {
    schema: &'a Schema,
    subgraph_names: Vec<String>,
    /// Tracks enum usage across subgraphs for proper enum merging
    enum_usages: HashMap<String, EnumTypeUsage>,
    /// Error reporting with subgraph context
    error_reporter: ErrorReporter,
    /// Subtyping rules configuration  
    allowed_subtyping_rules: SubtypingRules,
}

impl<'a> TypeMerger<'a> {
    /// Create a new TypeMerger with default configuration
    pub(crate) fn new(schema: &'a Schema, subgraph_names: Vec<String>) -> Self {
        Self {
            schema,
            subgraph_names: subgraph_names.clone(),
            enum_usages: HashMap::new(),
            error_reporter: ErrorReporter::new(subgraph_names),
            allowed_subtyping_rules: SubtypingRules::default(),
        }
    }

    /// Get the error reporter for accessing errors and hints
    pub(crate) fn error_reporter(&self) -> &ErrorReporter {
        &self.error_reporter
    }

    /// Get mutable access to the error reporter
    pub(crate) fn error_reporter_mut(&mut self) -> &mut ErrorReporter {
        &mut self.error_reporter
    }

    /// Check if there are any errors
    pub(crate) fn has_errors(&self) -> bool {
        self.error_reporter.has_errors()
    }

    /// Check if there are any hints
    pub(crate) fn has_hints(&self) -> bool {
        self.error_reporter.has_hints()
    }

    /// Get the errors and hints from the reporter
    pub(crate) fn into_errors_and_hints(self) -> (Vec<CompositionError>, Vec<CompositionHint>) {
        self.error_reporter.into_errors_and_hints()
    }

    /// Get enum usage for a specific enum type
    pub(crate) fn get_enum_usage(&self, enum_name: &str) -> Option<&EnumTypeUsage> {
        self.enum_usages.get(enum_name)
    }

    /// Get all enum usages
    pub(crate) fn enum_usages(&self) -> &HashMap<String, EnumTypeUsage> {
        &self.enum_usages
    }

    /// Core type merging logic
    ///
    /// Key differences from TypeScript implementation:
    /// - Uses generic type constraint `TElement: SchemaElementWithType` instead of Typescript
    ///   generic type parameters
    /// - Takes `&mut TElement` for explicit mutability vs TypeScript's implicit object mutation
    /// - Explicit lifetime management with `&mut self` instead of TypeScript's GC
    pub(crate) fn merge_type_reference<TElement>(
        &mut self,
        sources: &TypeSources,
        dest: &mut TElement,
        is_input_position: bool,
    ) -> Result<bool, FederationError>
    where
        TElement: SchemaElementWithType + 'a,
    {
        // Validate sources
        if sources.is_empty() {
            return Err(SingleFederationError::Internal {
                message: "No type sources provided for merging".to_string(),
            }
            .into());
        }

        let mut result_type: Option<Type> = None;
        let mut has_subtypes = false;
        let mut has_incompatible = false;

        // First pass: determine the merged type following GraphQL Federation variance rules
        for (_index, source_type) in type_sources_helpers::iter_types(sources) {
            let Some(source_type) = source_type else {
                continue;
            };

            if let Some(ref current_type) = result_type {
                if self.same_type(current_type, source_type) {
                    // Types are identical, continue
                    continue;
                } else if let Ok(true) = self.is_strict_subtype(current_type, source_type) {
                    // current_type is a subtype of source_type (source_type is more general)
                    has_subtypes = true;
                    if is_input_position {
                        // input: use more general (supertype) → upgrade to source_type
                        result_type = Some(source_type.clone());
                    }
                    // else output: keep current_type (the subtype)
                } else if let Ok(true) = self.is_strict_subtype(source_type, current_type) {
                    // source_type is a subtype of current_type (current_type is more general)
                    has_subtypes = true;
                    if !is_input_position {
                        // For input types, use the more specific type (contravariance)
                        result_type = Some(source_type.clone());
                    }
                    // else output: keep current_type (the supertype)
                } else {
                    // Types are incompatible
                    has_incompatible = true;
                }
            } else {
                // First type we encounter
                result_type = Some(source_type.clone());
            }
        }

        let Some(typ) = result_type else {
            // No sources provided - this shouldn't happen in normal operation
            return Err(FederationError::SingleFederationError(
                SingleFederationError::Internal {
                    message: format!(
                        "No source provided for {} across subgraphs",
                        dest.coordinate()
                    ),
                },
            ));
        };

        // Copy the type reference to the destination schema
        let copied_type = match self.copy_type_reference(&typ, self.schema) {
            Ok(copied) => copied,
            Err(_) => {
                // If copying fails, use the original type
                typ.clone()
            }
        };

        dest.set_type(copied_type);

        self.track_enum_usage(&typ, dest.coordinate(), is_input_position, sources);

        let element_kind = if is_input_position {
            "argument"
        } else {
            "field"
        };

        if has_incompatible {
            // Report incompatible type error
            let error_code_str = if is_input_position {
                "ARGUMENT_TYPE_MISMATCH"
            } else {
                "FIELD_TYPE_MISMATCH"
            };

            let error = CompositionError::InternalError {
                message: format!(
                    "Type of {} \"{}\" is incompatible across subgraphs",
                    element_kind,
                    dest.coordinate()
                ),
            };

            self.error_reporter.report_mismatch_error::<Type, ()>(
                error,
                &typ,
                sources,
                |typ, _is_supergraph| Some(format!("type \"{}\"", typ)),
            );

            Err(FederationError::SingleFederationError(
                SingleFederationError::Internal {
                    message: format!(
                        "Type of {} \"{}\" is incompatible across subgraphs",
                        element_kind,
                        dest.coordinate(),
                    ),
                },
            ))
        } else if has_subtypes {
            // Report compatibility hint for subtype relationships
            let hint_code = if is_input_position {
                HintCode::InconsistentButCompatibleArgumentType
            } else {
                HintCode::InconsistentButCompatibleFieldType
            };

            self.error_reporter.report_mismatch_hint::<Type, ()>(
                hint_code,
                format!(
                    "Type of {} \"{}\" is inconsistent but compatible across subgraphs:",
                    element_kind,
                    dest.coordinate()
                ),
                &typ,
                sources,
                |typ, _is_supergraph| Some(format!("type \"{}\"", typ)),
                false,
            );

            Err(FederationError::SingleFederationError(
                SingleFederationError::Internal {
                    message: format!(
                        "Type of {} \"{}\" is inconsistent but compatible across subgraphs",
                        element_kind,
                        dest.coordinate()
                    ),
                },
            ))
        } else {
            Ok(true)
        }
    }

    fn track_enum_usage(
        &mut self,
        typ: &Type,
        element_name: &str,
        is_input_position: bool,
        sources: &TypeSources,
    ) {
        // Get the base type (unwrap nullability and list wrappers)
        let base_type = self.get_base_type(typ);

        // Check if it's an enum type
        if let Some(&ExtendedType::Enum(_)) = self.schema.types.get(base_type.as_str()) {
            let enum_name = base_type.clone();

            // Get existing usage or create new one
            let existing = self.enum_usages.get(&enum_name).cloned();

            // Determine position based on input/output usage
            let this_position = if is_input_position { "Input" } else { "Output" };
            let position = if let Some(existing_usage) = &existing {
                match existing_usage {
                    EnumTypeUsage::Input { .. } if !is_input_position => "Both",
                    EnumTypeUsage::Output { .. } if is_input_position => "Both",
                    _ => this_position,
                }
            } else {
                this_position
            };

            let mut examples = match &existing {
                Some(EnumTypeUsage::Input { input_example }) => {
                    let mut examples = HashMap::new();
                    examples.insert("Input", input_example.clone());
                    examples
                }
                Some(EnumTypeUsage::Output { output_example }) => {
                    let mut examples = HashMap::new();
                    examples.insert("Output", output_example.clone());
                    examples
                }
                Some(EnumTypeUsage::Both {
                    input_example,
                    output_example,
                }) => {
                    let mut examples = HashMap::new();
                    examples.insert("Input", input_example.clone());
                    examples.insert("Output", output_example.clone());
                    examples
                }
                Some(EnumTypeUsage::Unused) => HashMap::new(),
                None => HashMap::new(),
            };

            // Add example for current position if not already present
            if !examples.contains_key(this_position) {
                // Find first non-null source
                let mut example_coordinate = element_name.to_string();
                for (idx, source_type) in type_sources_helpers::iter_types(sources) {
                    if source_type.is_some() {
                        // Use the coordinate from the first available source
                        // We need to get the subgraph name from the index
                        if let Some(subgraph_name) = self.subgraph_names.get(idx) {
                            example_coordinate = format!("{}@{}", element_name, subgraph_name);
                            break;
                        }
                    }
                }
                examples.insert(this_position, example_coordinate);
            }

            // Convert back to EnumTypeUsage format
            let new_usage = match position {
                "Input" => EnumTypeUsage::Input {
                    input_example: examples
                        .get("Input")
                        .unwrap_or(&element_name.to_string())
                        .clone(),
                },
                "Output" => EnumTypeUsage::Output {
                    output_example: examples
                        .get("Output")
                        .unwrap_or(&element_name.to_string())
                        .clone(),
                },
                "Both" => EnumTypeUsage::Both {
                    input_example: examples
                        .get("Input")
                        .unwrap_or(&element_name.to_string())
                        .clone(),
                    output_example: examples
                        .get("Output")
                        .unwrap_or(&element_name.to_string())
                        .clone(),
                },
                _ => unreachable!("Invalid position"),
            };

            // Store the updated usage
            self.enum_usages.insert(enum_name, new_usage);
        }
    }

    /// Extract the base type name from a type (removing nullability and list wrappers)
    fn get_base_type(&self, typ: &Type) -> String {
        get_base_type_impl(typ)
    }

    /// Check if two types are the same (including nullability and list structure) -  sameType
    fn same_type(&self, type1: &Type, type2: &Type) -> bool {
        same_type_impl(type1, type2)
    }

    /// Check if type1 is a strict subtype of type2
    fn is_strict_subtype(&self, type1: &Type, type2: &Type) -> Result<bool, FederationError> {
        // Use the allowed subtyping rules to determine if the relationship is valid
        let allowed_rules = &self.allowed_subtyping_rules.allowed_rules;

        match type1 {
            Type::List(inner1) => {
                if !allowed_rules.contains(&SubtypingRule::ListPropagation) {
                    return Ok(false);
                }
                match type2 {
                    Type::List(inner2) => self.is_strict_subtype(inner1, inner2),
                    _ => Ok(false),
                }
            }
            Type::NonNullList(inner1) => match type2 {
                Type::NonNullList(inner2) => {
                    if !allowed_rules.contains(&SubtypingRule::NonNullablePropagation) {
                        return Ok(false);
                    }
                    self.is_strict_subtype(inner1, inner2)
                }
                Type::List(inner2) => {
                    if !allowed_rules.contains(&SubtypingRule::NonNullableDowngrade) {
                        return Ok(false);
                    }
                    self.is_strict_subtype(inner1, inner2)
                }
                _ => Ok(false),
            },
            Type::Named(n1) => {
                match type2 {
                    Type::List(inner2) => {
                        if !allowed_rules.contains(&SubtypingRule::ListUpgrade) {
                            return Ok(false);
                        }
                        self.is_strict_subtype(type1, inner2)
                    }
                    Type::NonNullList(inner2) => {
                        if !allowed_rules.contains(&SubtypingRule::ListUpgrade) {
                            return Ok(false);
                        }
                        self.is_strict_subtype(type1, inner2)
                    }
                    Type::Named(n2) => {
                        if n1 == n2 {
                            return Ok(false); // Same type, not a strict subtype
                        }
                        if !allowed_rules.contains(&SubtypingRule::Direct) {
                            return Ok(false);
                        }
                        self.is_named_type_subtype(n1, n2)
                    }
                    Type::NonNullNamed(n2) => {
                        if n1 == n2 {
                            return Ok(false); // Same type, not a strict subtype
                        }
                        if !allowed_rules.contains(&SubtypingRule::Direct) {
                            return Ok(false);
                        }
                        self.is_named_type_subtype(n1, n2)
                    }
                }
            }
            Type::NonNullNamed(n1) => {
                match type2 {
                    Type::List(inner2) => {
                        if !allowed_rules.contains(&SubtypingRule::ListUpgrade) {
                            return Ok(false);
                        }
                        self.is_strict_subtype(type1, inner2)
                    }
                    Type::NonNullList(inner2) => {
                        if !allowed_rules.contains(&SubtypingRule::ListUpgrade) {
                            return Ok(false);
                        }
                        self.is_strict_subtype(type1, inner2)
                    }
                    Type::Named(n2) => {
                        if !allowed_rules.contains(&SubtypingRule::NonNullableDowngrade) {
                            return Ok(false);
                        }
                        if n1 == n2 {
                            return Ok(true); // NonNull T is subtype of T
                        }
                        self.is_named_type_subtype(n1, n2)
                    }
                    Type::NonNullNamed(n2) => {
                        if n1 == n2 {
                            return Ok(false); // Same type, not a strict subtype
                        }
                        if !allowed_rules.contains(&SubtypingRule::Direct) {
                            return Ok(false);
                        }
                        self.is_named_type_subtype(n1, n2)
                    }
                }
            }
        }
    }

    /// Check if named_type1 is a subtype of named_type2 (interface/union relationships)
    fn is_named_type_subtype(
        &self,
        subtype: &NamedType,
        supertype: &NamedType,
    ) -> Result<bool, FederationError> {
        let subtype_def =
            self.schema
                .types
                .get(subtype)
                .ok_or_else(|| SingleFederationError::Internal {
                    message: format!("Cannot find type '{}' in schema", subtype),
                })?;

        let supertype_def =
            self.schema
                .types
                .get(supertype)
                .ok_or_else(|| SingleFederationError::Internal {
                    message: format!("Cannot find type '{}' in schema", supertype),
                })?;

        if !self
            .allowed_subtyping_rules
            .allowed_rules
            .contains(&SubtypingRule::Direct)
        {
            return Ok(false);
        }
        match (subtype_def, supertype_def) {
            // Object type implementing an interface
            (ExtendedType::Object(obj), ExtendedType::Interface(_)) => {
                Ok(obj.implements_interfaces.contains(supertype))
            }
            // Interface extending another interface
            (ExtendedType::Interface(sub_intf), ExtendedType::Interface(_)) => {
                Ok(sub_intf.implements_interfaces.contains(supertype))
            }
            // Object type that is a member of a union
            (ExtendedType::Object(_), ExtendedType::Union(union_type)) => {
                Ok(union_type.members.contains(subtype))
            }
            // Interface that is a member of a union (if supported)
            (ExtendedType::Interface(_), ExtendedType::Union(union_type)) => {
                Ok(union_type.members.contains(subtype))
            }
            _ => Ok(false),
        }
    }

    /// Copy a type reference from one schema to another, resolving named types
    pub(crate) fn copy_type_reference(
        &self,
        source_type: &Type,
        target_schema: &Schema,
    ) -> Result<Type, FederationError> {
        copy_type_reference_impl(source_type, target_schema)
    }
}

/// Copy a type reference from one schema to another, resolving named types
fn copy_type_reference_impl(
    source_type: &Type,
    target_schema: &Schema,
) -> Result<Type, FederationError> {
    match source_type {
        Type::Named(name) => {
            // Verify the type exists in target schema
            if target_schema.types.contains_key(name) {
                Ok(Type::Named(name.clone()))
            } else {
                Err(SingleFederationError::Internal {
                    message: format!("Cannot find type '{}' in target schema", name),
                }
                .into())
            }
        }
        Type::NonNullNamed(name) => {
            // Verify the type exists in target schema
            if target_schema.types.contains_key(name) {
                Ok(Type::NonNullNamed(name.clone()))
            } else {
                Err(SingleFederationError::Internal {
                    message: format!("Cannot find type '{}' in target schema", name),
                }
                .into())
            }
        }
        Type::List(inner) => {
            let copied_inner = copy_type_reference_impl(inner, target_schema)?;
            Ok(Type::List(Box::new(copied_inner)))
        }
        Type::NonNullList(inner) => {
            let copied_inner = copy_type_reference_impl(inner, target_schema)?;
            Ok(Type::NonNullList(Box::new(copied_inner)))
        }
    }
}

/// Extract the base type name from a type (removing nullability and list wrappers)
fn get_base_type_impl(typ: &Type) -> String {
    match typ {
        Type::Named(name) => name.to_string(),
        Type::NonNullNamed(name) => name.to_string(),
        Type::List(inner) => get_base_type_impl(inner),
        Type::NonNullList(inner) => get_base_type_impl(inner),
    }
}

/// Check if two types are the same (including nullability and list structure)
fn same_type_impl(type1: &Type, type2: &Type) -> bool {
    match (type1, type2) {
        (Type::Named(n1), Type::Named(n2)) => n1 == n2,
        (Type::NonNullNamed(n1), Type::NonNullNamed(n2)) => n1 == n2,
        (Type::List(inner1), Type::List(inner2)) => same_type_impl(inner1, inner2),
        (Type::NonNullList(inner1), Type::NonNullList(inner2)) => same_type_impl(inner1, inner2),
        _ => false,
    }
}

#[cfg(test)]
mod type_merging_tests {
    use apollo_compiler::Name;
    use apollo_compiler::Node;
    use apollo_compiler::ast::FieldDefinition;
    use apollo_compiler::ast::InputValueDefinition;
    use apollo_compiler::schema::ComponentName;
    use apollo_compiler::schema::EnumType;
    use apollo_compiler::schema::InterfaceType;
    use apollo_compiler::schema::ObjectType;
    use apollo_compiler::schema::UnionType;

    use super::*;

    /// Test helper struct for type merging tests
    /// In production, this trait is implemented by real schema elements like FieldDefinition and InputValueDefinition
    #[derive(Debug, Clone)]
    pub(crate) struct TestSchemaElement {
        pub(crate) coordinate: String,
        pub(crate) typ: Option<Type>,
    }

    impl SchemaElementWithType for TestSchemaElement {
        fn coordinate(&self) -> &str {
            &self.coordinate
        }

        fn get_type(&self) -> Option<&Type> {
            self.typ.as_ref()
        }

        fn set_type(&mut self, typ: Type) {
            self.typ = Some(typ);
        }
    }

    fn create_test_schema() -> Schema {
        let mut schema = Schema::new();

        // Add interface I
        let interface_type = InterfaceType {
            description: None,
            name: Name::new("I").unwrap(),
            implements_interfaces: Default::default(),
            directives: Default::default(),
            fields: Default::default(),
        };
        schema.types.insert(
            Name::new("I").unwrap(),
            ExtendedType::Interface(Node::new(interface_type)),
        );

        // Add object type A implementing I
        let mut object_type = ObjectType {
            description: None,
            name: Name::new("A").unwrap(),
            implements_interfaces: Default::default(),
            directives: Default::default(),
            fields: Default::default(),
        };
        object_type
            .implements_interfaces
            .insert(ComponentName::from(Name::new("I").unwrap()));
        schema.types.insert(
            Name::new("A").unwrap(),
            ExtendedType::Object(Node::new(object_type)),
        );

        // Add union U with member A
        let mut union_type = UnionType {
            description: None,
            name: Name::new("U").unwrap(),
            directives: Default::default(),
            members: Default::default(),
        };
        union_type
            .members
            .insert(ComponentName::from(Name::new("A").unwrap()));
        schema.types.insert(
            Name::new("U").unwrap(),
            ExtendedType::Union(Node::new(union_type)),
        );

        // Add enum Status for enum usage tracking tests
        let enum_type = EnumType {
            description: None,
            name: Name::new("Status").unwrap(),
            directives: Default::default(),
            values: Default::default(),
        };
        schema.types.insert(
            Name::new("Status").unwrap(),
            ExtendedType::Enum(Node::new(enum_type)),
        );

        schema
    }

    #[test]
    fn same_types() {
        let schema = create_test_schema();
        let mut merger = TypeMerger::new(
            &schema,
            vec!["subgraph1".to_string(), "subgraph2".to_string()],
        );

        let mut sources = type_sources_helpers::new(2);
        type_sources_helpers::set_source(
            &mut sources,
            0,
            Type::Named(Name::new("String").unwrap()),
        );
        type_sources_helpers::set_source(
            &mut sources,
            1,
            Type::Named(Name::new("String").unwrap()),
        );

        let result = merger.merge_type_reference(
            &sources,
            &mut TestSchemaElement {
                coordinate: "testField".to_string(),
                typ: None,
            },
            false,
        );

        // Check that there are no errors or hints
        assert!(!merger.has_errors());
        assert_eq!(merger.enum_usages().len(), 0);
    }

    #[test]
    fn nullable_vs_non_nullable() {
        let schema = create_test_schema();
        let mut merger = TypeMerger::new(
            &schema,
            vec!["subgraph1".to_string(), "subgraph2".to_string()],
        );

        let mut sources = type_sources_helpers::new(2);
        type_sources_helpers::set_source(
            &mut sources,
            0,
            Type::NonNullNamed(Name::new("String").unwrap()),
        );
        type_sources_helpers::set_source(
            &mut sources,
            1,
            Type::Named(Name::new("String").unwrap()),
        );

        // For output types, should use the more general type (nullable)
        let result = merger.merge_type_reference(
            &sources,
            &mut TestSchemaElement {
                coordinate: "testField".to_string(),
                typ: None,
            },
            false,
        );
        // Check that there are no errors but there might be hints
        assert!(!merger.has_errors());
        assert_eq!(merger.enum_usages().len(), 0);

        // Create a new merger for the next test since we can't clear the reporter
        let mut merger = TypeMerger::new(
            &schema,
            vec!["subgraph1".to_string(), "subgraph2".to_string()],
        );

        // For input types, should use the more specific type (non-nullable)
        let _result = merger.merge_type_reference(
            &sources,
            &mut TestSchemaElement {
                coordinate: "testArg".to_string(),
                typ: None,
            },
            true,
        );
        // Check that there are no errors but there might be hints
        assert!(!merger.has_errors());
        assert_eq!(merger.enum_usages().len(), 0);
    }

    #[test]
    fn interface_subtype() {
        let schema = create_test_schema();
        let mut merger = TypeMerger::new(
            &schema,
            vec!["subgraph1".to_string(), "subgraph2".to_string()],
        );

        let mut sources = type_sources_helpers::new(2);
        type_sources_helpers::set_source(&mut sources, 0, Type::Named(Name::new("I").unwrap()));
        type_sources_helpers::set_source(&mut sources, 1, Type::Named(Name::new("A").unwrap()));

        // For output types, should use the more general type (interface)
        let result = merger.merge_type_reference(
            &sources,
            &mut TestSchemaElement {
                coordinate: "testField".to_string(),
                typ: None,
            },
            false,
        );
        // Check that there are no errors but there might be hints
        assert!(!merger.has_errors());
        assert_eq!(merger.enum_usages().len(), 0);

        // For input types, should use the more specific type (implementing type)
        let _result = merger.merge_type_reference(
            &sources,
            &mut TestSchemaElement {
                coordinate: "testArg".to_string(),
                typ: None,
            },
            true,
        );
        // Check that there are no errors but there might be hints
        assert!(!merger.has_errors());
        assert_eq!(merger.enum_usages().len(), 0);
    }

    #[test]
    fn incompatible_types() {
        let schema = create_test_schema();
        let mut merger = TypeMerger::new(
            &schema,
            vec!["subgraph1".to_string(), "subgraph2".to_string()],
        );

        let mut sources = type_sources_helpers::new(2);
        type_sources_helpers::set_source(
            &mut sources,
            0,
            Type::Named(Name::new("String").unwrap()),
        );
        type_sources_helpers::set_source(&mut sources, 1, Type::Named(Name::new("Int").unwrap()));

        let _result = merger.merge_type_reference(
            &sources,
            &mut TestSchemaElement {
                coordinate: "testField".to_string(),
                typ: None,
            },
            false,
        );
        // Check that there are errors for incompatible types
        assert!(merger.has_errors());
        assert_eq!(merger.enum_usages().len(), 0);
    }

    #[test]
    fn enum_usage_tracking() {
        let schema = create_test_schema();
        let mut merger = TypeMerger::new(
            &schema,
            vec!["subgraph1".to_string(), "subgraph2".to_string()],
        );

        // Test enum usage in output position
        let mut sources = type_sources_helpers::new(2);
        type_sources_helpers::set_source(
            &mut sources,
            0,
            Type::Named(Name::new("Status").unwrap()),
        );
        type_sources_helpers::set_source(
            &mut sources,
            1,
            Type::Named(Name::new("Status").unwrap()),
        );

        let _ = merger.merge_type_reference(
            &sources,
            &mut TestSchemaElement {
                coordinate: "user_status".to_string(),
                typ: None,
            },
            false,
        );

        // Test enum usage in input position
        let mut arg_sources = type_sources_helpers::new(2);
        type_sources_helpers::set_source(
            &mut arg_sources,
            0,
            Type::Named(Name::new("Status").unwrap()),
        );
        type_sources_helpers::set_source(
            &mut arg_sources,
            1,
            Type::Named(Name::new("Status").unwrap()),
        );

        let _ = merger.merge_type_reference(
            &arg_sources,
            &mut TestSchemaElement {
                coordinate: "status_filter".to_string(),
                typ: None,
            },
            true,
        );

        // Verify enum usage tracking
        let enum_usage = merger.get_enum_usage("Status");
        assert!(enum_usage.is_some());

        let usage = enum_usage.unwrap();
        match usage {
            EnumTypeUsage::Both {
                input_example,
                output_example,
            } => {
                assert_eq!(input_example, "status_filter@subgraph1");
                assert_eq!(output_example, "user_status@subgraph1");
            }
            _ => panic!("Expected Both usage, got {:?}", usage),
        }
    }

    #[test]
    fn enum_usage_input_only() {
        let schema = create_test_schema();
        let mut merger = TypeMerger::new(
            &schema,
            vec!["subgraph1".to_string(), "subgraph2".to_string()],
        );

        // Test enum usage only in input position
        let mut sources = type_sources_helpers::new(2);
        type_sources_helpers::set_source(
            &mut sources,
            0,
            Type::Named(Name::new("Status").unwrap()),
        );
        type_sources_helpers::set_source(
            &mut sources,
            1,
            Type::Named(Name::new("Status").unwrap()),
        );

        let _ = merger.merge_type_reference(
            &sources,
            &mut TestSchemaElement {
                coordinate: "status_filter".to_string(),
                typ: None,
            },
            true,
        );

        // Verify enum usage tracking
        let enum_usage = merger.get_enum_usage("Status");
        assert!(enum_usage.is_some());

        let usage = enum_usage.unwrap();
        match usage {
            EnumTypeUsage::Input { input_example } => {
                assert_eq!(input_example, "status_filter@subgraph1");
            }
            _ => panic!("Expected Input usage, got {:?}", usage),
        }
    }

    #[test]
    fn enum_usage_output_only() {
        let schema = create_test_schema();
        let mut merger = TypeMerger::new(
            &schema,
            vec!["subgraph1".to_string(), "subgraph2".to_string()],
        );

        // Test enum usage only in output position
        let mut sources = type_sources_helpers::new(2);
        type_sources_helpers::set_source(
            &mut sources,
            0,
            Type::Named(Name::new("Status").unwrap()),
        );
        type_sources_helpers::set_source(
            &mut sources,
            1,
            Type::Named(Name::new("Status").unwrap()),
        );

        let _ = merger.merge_type_reference(
            &sources,
            &mut TestSchemaElement {
                coordinate: "user_status".to_string(),
                typ: None,
            },
            false,
        );

        // Verify enum usage tracking
        let enum_usage = merger.get_enum_usage("Status");
        assert!(enum_usage.is_some());

        let usage = enum_usage.unwrap();
        match usage {
            EnumTypeUsage::Output { output_example } => {
                assert_eq!(output_example, "user_status@subgraph1");
            }
            _ => panic!("Expected Output usage, got {:?}", usage),
        }
    }

    #[test]
    fn panics_when_no_source_has_defined_type() {
        let schema = create_test_schema();
        let mut merger = TypeMerger::new(&schema, vec!["s1".to_string(), "s2".to_string()]);

        let sources = type_sources_helpers::new(2);
        // both entries None by default

        let mut element = TestSchemaElement {
            coordinate: "f".into(),
            typ: None,
        };
        // should panic
        let result = merger.merge_type_reference(&sources, &mut element, false);
        assert!(
            result.is_err(),
            "Expected merge to fail with no defined types"
        );
    }

    #[test]
    fn field_definition_not_argument_defaults_to_output() {
        let schema = create_test_schema();
        let mut merger = TypeMerger::new(&schema, vec!["s1".to_string()]);

        let mut sources = type_sources_helpers::new(1);
        type_sources_helpers::set_source(&mut sources, 0, Type::Named(Name::new("F").unwrap()));

        let mut element = TestSchemaElement {
            coordinate: "f".into(),
            typ: None,
        };
        let res = merger.merge_type_reference(&sources, &mut element, false);
        assert!(
            res.is_ok(),
            "Expected merge to succeed for field definition"
        );
        assert_eq!(element.typ.unwrap().to_string(), "F");
        // enum_usages remains empty for non-enum
        assert!(merger.enum_usages().is_empty());
    }

    #[test]
    fn strict_subtype_interface_object() {
        // Object A implementing interface I => A <: I
        let schema = create_test_schema();
        let merger = TypeMerger::new(&schema, vec!["s1".into(), "s2".into()]);
        let iface = Type::Named(Name::new("I").unwrap());
        let obj = Type::Named(Name::new("A").unwrap());
        assert!(merger.is_strict_subtype(&obj, &iface).unwrap());
        assert!(!merger.is_strict_subtype(&iface, &obj).unwrap());
    }

    #[test]
    fn strict_subtype_union_membership() {
        // Member A of union U => A <: U
        let schema = create_test_schema();
        let merger = TypeMerger::new(&schema, vec!["s1".into()]);
        let member = Type::Named(Name::new("A").unwrap());
        let union = Type::Named(Name::new("U").unwrap());
        assert!(merger.is_strict_subtype(&member, &union).unwrap());
    }

    #[test]
    fn skips_undefined_sources() {
        let schema = create_test_schema();
        let mut merger = TypeMerger::new(
            &schema,
            vec![
                "subgraph1".to_string(),
                "subgraph2".to_string(),
                "subgraph3".to_string(),
            ],
        );

        // subgraph 1 and 3 define T, subgraph 2 is None
        let mut sources = type_sources_helpers::new(3);
        type_sources_helpers::set_source(&mut sources, 0, Type::Named(Name::new("T").unwrap()));
        // index 1 left None
        type_sources_helpers::set_source(&mut sources, 2, Type::Named(Name::new("T").unwrap()));

        let mut element = TestSchemaElement {
            coordinate: "f".into(),
            typ: None,
        };
        let result = merger.merge_type_reference(&sources, &mut element, false);
        assert!(
            result.is_ok(),
            "Expected merge to succeed with undefined sources"
        );
        assert!(!merger.has_errors());
        assert_eq!(element.typ.unwrap().to_string(), "T");
    }

    #[test]
    fn interface_subtype_behavior() {
        let schema = create_test_schema();
        let mut merger = TypeMerger::new(&schema, vec!["s1".to_string(), "s2".to_string()]);

        // A implements I
        let a = Type::Named(Name::new("A").unwrap());
        let i = Type::Named(Name::new("I").unwrap());
        let mut sources = type_sources_helpers::new(2);
        type_sources_helpers::set_source(&mut sources, 0, a.clone());
        type_sources_helpers::set_source(&mut sources, 1, i.clone());

        // Output: pick subtype A
        let mut elem_out = TestSchemaElement {
            coordinate: "f".into(),
            typ: None,
        };
        let res_out = merger.merge_type_reference(&sources, &mut elem_out, false);
        assert!(res_out.is_err());
        assert_eq!(elem_out.typ.unwrap().to_string(), "A");
        assert!(
            merger.has_hints(),
            "Expected a compatibility hint for deep subtype chain"
        );

        // Input: pick supertype I
        let mut elem_in = TestSchemaElement {
            coordinate: "arg".into(),
            typ: None,
        };
        let res_in = merger.merge_type_reference(&sources, &mut elem_in, true);
        assert!(res_in.is_err());
        assert_eq!(elem_in.typ.unwrap().to_string(), "I");
        assert!(
            merger.has_hints(),
            "Expected a compatibility hint for deep subtype chain"
        );
    }

    #[test]
    fn list_and_interface_covariance() {
        let schema = create_test_schema();
        let mut merger = TypeMerger::new(&schema, vec!["s1".to_string(), "s2".to_string()]);

        // A implements interface I
        let a = Type::Named(Name::new("A").unwrap());
        let i = Type::Named(Name::new("I").unwrap());
        let list_a = Type::List(Box::new(a.clone()));
        let list_i = Type::List(Box::new(i.clone()));

        let mut sources = type_sources_helpers::new(2);
        type_sources_helpers::set_source(&mut sources, 0, list_a.clone());
        type_sources_helpers::set_source(&mut sources, 1, list_i.clone());

        // Output: pick subtype [A]
        let mut out = TestSchemaElement {
            coordinate: "f".into(),
            typ: None,
        };
        let res_out = merger.merge_type_reference(&sources, &mut out, false);
        assert!(res_out.is_err());
        assert_eq!(out.typ.unwrap().to_string(), "[A]");

        // Input: pick supertype [I]
        let mut inp = TestSchemaElement {
            coordinate: "arg".into(),
            typ: None,
        };
        let res_in = merger.merge_type_reference(&sources, &mut inp, true);
        assert!(res_in.is_err());
        assert_eq!(inp.typ.unwrap().to_string(), "[I]");
    }

    #[test]
    fn nested_list_covariance() {
        let schema = create_test_schema();
        let mut merger = TypeMerger::new(&schema, vec!["s1".to_string(), "s2".to_string()]);

        // Test nested lists: [[A]] vs [[I]]
        let a = Type::Named(Name::new("A").unwrap());
        let i = Type::Named(Name::new("I").unwrap());
        let list_a = Type::List(Box::new(a.clone()));
        let list_i = Type::List(Box::new(i.clone()));
        let list2_a = Type::List(Box::new(list_a.clone()));
        let list2_i = Type::List(Box::new(list_i.clone()));

        let mut sources = type_sources_helpers::new(2);
        type_sources_helpers::set_source(&mut sources, 0, list2_a.clone());
        type_sources_helpers::set_source(&mut sources, 1, list2_i.clone());

        // Output: pick subtype [[A]]
        let mut out = TestSchemaElement {
            coordinate: "f".into(),
            typ: None,
        };
        let res_out = merger.merge_type_reference(&sources, &mut out, false);
        assert!(res_out.is_err());
        assert_eq!(out.typ.unwrap().to_string(), "[[A]]");

        // Input: pick supertype [[I]]
        let mut inp = TestSchemaElement {
            coordinate: "arg".into(),
            typ: None,
        };
        let res_in = merger.merge_type_reference(&sources, &mut inp, true);
        assert!(res_in.is_err());
        assert_eq!(inp.typ.unwrap().to_string(), "[[I]]");
    }

    #[test]
    fn merge_with_field_definition_element() {
        let schema = create_test_schema();
        let mut merger = TypeMerger::new(&schema, vec!["s1".to_string()]);

        // Prepare a field definition in the schema
        let mut field_def = FieldDefinition {
            name: Name::new("field").unwrap(),
            description: None,
            arguments: vec![],
            directives: Default::default(),
            ty: Type::Named(Name::new("String").unwrap()),
        };
        let mut sources = type_sources_helpers::new(1);
        type_sources_helpers::set_source(
            &mut sources,
            0,
            Type::Named(Name::new("String").unwrap()),
        );

        // Call merge_type_reference on a FieldDefinition (TElement = FieldDefinition)
        let res = merger.merge_type_reference(&sources, &mut field_def, false);
        assert!(
            res.is_ok(),
            "Merging identical types on a FieldDefinition should return true"
        );
        assert_eq!(
            match field_def.ty.clone() {
                Type::Named(n) => n.to_string(),
                _ => String::new(),
            },
            "String"
        );
    }

    #[test]
    fn merge_with_input_value_definition_element() {
        let schema = create_test_schema();
        let mut merger = TypeMerger::new(&schema, vec!["s1".to_string(), "s2".to_string()]);

        // Prepare an input value definition (argument) type
        let mut input_def = InputValueDefinition {
            name: Name::new("arg").unwrap(),
            description: None,
            default_value: None,
            directives: Default::default(),
            ty: Type::Named(Name::new("Int").unwrap()).into(),
        };
        let mut sources = type_sources_helpers::new(2);
        type_sources_helpers::set_source(&mut sources, 0, Type::Named(Name::new("Int").unwrap()));
        type_sources_helpers::set_source(
            &mut sources,
            1,
            Type::NonNullNamed(Name::new("Int").unwrap()),
        );

        // In input position, non-null should be overridden by nullable
        let res = merger.merge_type_reference(&sources, &mut input_def, true);
        assert!(
            res.is_err(),
            "In input position merging nullable vs non-null should hint"
        );
        assert_eq!(
            match input_def.ty.as_ref() {
                Type::Named(n) => n.as_str(),
                _ => "",
            },
            "Int"
        );
    }
}
