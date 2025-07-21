// this is temporary as we are porting the type merging logic from the TypeScript implementation
// we will remove this once we have integrated it to the main codebase
#![allow(dead_code)]
#![allow(unused_variables)]

use std::collections::HashMap;

use apollo_compiler::Node;
use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::ast::InputValueDefinition;
use apollo_compiler::ast::NamedType;
use apollo_compiler::ast::Type;
use apollo_compiler::schema::ExtendedType;

use crate::error::CompositionError;
use crate::error::FederationError;
use crate::error::SingleFederationError;
use crate::merger::EnumTypeUsage;
use crate::merger::hints::HintCode;
use crate::merger::merge::Sources;
use crate::merger::merge_enum::EnumExample;
use crate::merger::merge_enum::EnumExampleAst;

pub(crate) trait SchemaElementWithType {
    fn coordinate(&self) -> &str;
    fn get_type(&self) -> Option<&Type>;
    fn set_type(&mut self, typ: Type);
    fn enum_example_ast(&self) -> Option<EnumExampleAst>;
}

impl SchemaElementWithType for FieldDefinition {
    fn coordinate(&self) -> &str {
        // could also include parent type name if you have it; for now:
        self.name.as_str()
    }
    fn get_type(&self) -> Option<&Type> {
        Some(&self.ty)
    }
    fn set_type(&mut self, typ: Type) {
        self.ty = typ;
    }
    fn enum_example_ast(&self) -> Option<EnumExampleAst> {
        Some(EnumExampleAst::Field(Node::new(self.clone())))
    }
}

impl SchemaElementWithType for InputValueDefinition {
    fn coordinate(&self) -> &str {
        self.name.as_str()
    }
    fn get_type(&self) -> Option<&Type> {
        Some(&self.ty)
    }
    fn set_type(&mut self, typ: Type) {
        self.ty = typ.into();
    }
    fn enum_example_ast(&self) -> Option<EnumExampleAst> {
        Some(EnumExampleAst::Input(Node::new(self.clone())))
    }
}

/// Type alias for tracking sources of a type across different subgraphs
/// Using the existing Sources<T> pattern from the merger module
pub(crate) type TypeSources = Sources<Type>;

use crate::merger::merge::Merger;

impl Merger {
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
        TElement: SchemaElementWithType,
    {
        // Validate sources
        if sources.is_empty() {
            self.error_reporter_mut()
                .add_error(CompositionError::InternalError {
                    message: "No type sources provided for merging".to_string(),
                });
            return Ok(false);
        }

        let mut result_type: Option<Type> = None;
        let mut has_subtypes = false;
        let mut has_incompatible = false;

        // First pass: determine the merged type following GraphQL Federation variance rules
        for (_index, source_type) in sources.iter().map(|(idx, typ)| (*idx, typ.as_ref())) {
            let Some(source_type) = source_type else {
                continue;
            };

            if let Some(ref current_type) = result_type {
                if Self::same_type(current_type, source_type) {
                    // Types are identical, continue
                    continue;
                } else if let Ok(true) = self.is_strict_subtype(source_type, current_type) {
                    // current_type is a subtype of source_type (source_type is more general)
                    has_subtypes = true;
                    if is_input_position {
                        // input: use more general (supertype) → upgrade to source_type
                        result_type = Some(source_type.clone());
                    }
                    // else output: keep current_type (the subtype)
                } else if let Ok(true) = self.is_strict_subtype(current_type, source_type) {
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
            let error = CompositionError::InternalError {
                message: format!(
                    "No type sources provided for {} across subgraphs",
                    dest.coordinate()
                ),
            };
            self.error_reporter_mut().add_error(error);

            return Ok(false);
        };

        // Copy the type reference to the destination schema
        let copied_type = self.copy_type_reference(&typ)?;

        dest.set_type(copied_type);

        if let Some(ast_node) = dest.enum_example_ast() {
            self.track_enum_usage(
                &typ,
                dest.coordinate(),
                ast_node,
                is_input_position,
                sources,
            );
        }

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

            self.error_reporter_mut().report_mismatch_error::<Type, ()>(
                error,
                &typ,
                sources,
                |typ, _is_supergraph| Some(format!("type \"{}\"", typ)),
            );

            Ok(false)
        } else if has_subtypes {
            // Report compatibility hint for subtype relationships
            let hint_code = if is_input_position {
                HintCode::InconsistentButCompatibleArgumentType
            } else {
                HintCode::InconsistentButCompatibleFieldType
            };

            self.error_reporter_mut().report_mismatch_hint::<Type, ()>(
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

            Ok(false)
        } else {
            Ok(true)
        }
    }

    fn track_enum_usage(
        &mut self,
        typ: &Type,
        element_name: &str,
        element_ast: EnumExampleAst,
        is_input_position: bool,
        sources: &TypeSources,
    ) {
        // Get the base type (unwrap nullability and list wrappers)
        let base_type_name = typ.inner_named_type();

        // Check if it's an enum type
        if let Some(&ExtendedType::Enum(_)) = self.schema().schema().types.get(base_type_name) {
            let enum_name = base_type_name.to_string();

            // Get existing usage or create new one
            let existing = self.enum_usages().get(&enum_name).cloned();

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
                // For enum types, the coordinate is just the element name (field coordinate)
                // Since element_name comes from dest.coordinate(), it's already in correct format
                examples.insert(
                    this_position,
                    EnumExample {
                        coordinate: element_name.to_string(),
                        element_ast: Some(element_ast.clone()),
                    },
                );
            }

            // Convert back to EnumTypeUsage format
            let new_usage = match position {
                "Input" => EnumTypeUsage::Input {
                    input_example: examples
                        .get("Input")
                        .cloned()
                        .unwrap_or_else(|| EnumExample {
                            coordinate: element_name.to_string(),
                            element_ast: Some(element_ast.clone()),
                        })
                        .clone(),
                },
                "Output" => EnumTypeUsage::Output {
                    output_example: examples
                        .get("Output")
                        .cloned()
                        .unwrap_or_else(|| EnumExample {
                            coordinate: element_name.to_string(),
                            element_ast: Some(element_ast.clone()),
                        })
                        .clone(),
                },
                "Both" => EnumTypeUsage::Both {
                    input_example: examples
                        .get("Input")
                        .cloned()
                        .unwrap_or_else(|| EnumExample {
                            coordinate: element_name.to_string(),
                            element_ast: Some(element_ast.clone()),
                        })
                        .clone(),
                    output_example: examples
                        .get("Output")
                        .cloned()
                        .unwrap_or_else(|| EnumExample {
                            coordinate: element_name.to_string(),
                            element_ast: Some(element_ast.clone()),
                        })
                        .clone(),
                },
                _ => unreachable!("Invalid position"),
            };

            // Store the updated usage
            self.enum_usages_mut().insert(enum_name, new_usage);
        }
    }

    fn same_type(dest_type: &Type, source_type: &Type) -> bool {
        match (dest_type, source_type) {
            (Type::Named(n1), Type::Named(n2)) => n1 == n2,
            (Type::NonNullNamed(n1), Type::NonNullNamed(n2)) => n1 == n2,
            (Type::List(inner1), Type::List(inner2)) => Self::same_type(inner1, inner2),
            (Type::NonNullList(inner1), Type::NonNullList(inner2)) => {
                Self::same_type(inner1, inner2)
            }
            _ => false,
        }
    }

    fn is_strict_subtype(
        &self,
        potential_supertype: &Type,
        potential_subtype: &Type,
    ) -> Result<bool, FederationError> {
        // Hardcoded subtyping rules based on the default configuration:
        // - Direct: Interface/union subtyping relationships
        // - NonNullableDowngrade: NonNull T is subtype of T
        // - ListPropagation: [T] is subtype of [U] if T is subtype of U
        // - NonNullablePropagation: NonNull T is subtype of NonNull U if T is subtype of U
        // - ListUpgrade is NOT supported (was excluded by default)

        match potential_subtype {
            Type::List(inner_sub) => match potential_supertype {
                Type::List(inner_super) => {
                    // ListPropagation: [T] is subtype of [U] if T is subtype of U
                    self.is_strict_subtype(inner_super, inner_sub)
                }
                _ => Ok(false),
            },
            Type::NonNullList(inner_sub) => match potential_supertype {
                Type::NonNullList(inner_super) => {
                    // NonNullablePropagation: [T]! is subtype of [U]! if T is subtype of U
                    self.is_strict_subtype(inner_super, inner_sub)
                }
                Type::List(inner_super) => {
                    // NonNullableDowngrade: [T]! is subtype of [T]
                    self.is_strict_subtype(inner_super, inner_sub)
                }
                _ => Ok(false),
            },
            Type::Named(name_sub) => match potential_supertype {
                Type::Named(name_super) => {
                    if name_sub == name_super {
                        return Ok(false); // Same type, not a strict subtype
                    }
                    // Direct: Interface/union subtyping relationships
                    self.is_named_type_subtype(name_super, name_sub)
                }
                Type::NonNullNamed(name_super) => {
                    if name_sub == name_super {
                        return Ok(false); // Same type, not a strict subtype
                    }
                    // Direct: Interface/union subtyping relationships
                    self.is_named_type_subtype(name_super, name_sub)
                }
                // ListUpgrade not supported: T is NOT a subtype of [T]
                _ => Ok(false),
            },
            Type::NonNullNamed(name_sub) => match potential_supertype {
                Type::Named(name_super) => {
                    if name_sub == name_super {
                        return Ok(true); // NonNull T is subtype of T (NonNullableDowngrade)
                    }
                    // Direct: Interface/union subtyping relationships with NonNullableDowngrade
                    self.is_named_type_subtype(name_super, name_sub)
                }
                Type::NonNullNamed(name_super) => {
                    if name_sub == name_super {
                        return Ok(false); // Same type, not a strict subtype
                    }
                    // Direct: Interface/union subtyping relationships
                    self.is_named_type_subtype(name_super, name_sub)
                }
                // ListUpgrade not supported: T! is NOT a subtype of [T] or [T]!
                _ => Ok(false),
            },
        }
    }

    fn is_named_type_subtype(
        &self,
        potential_supertype: &NamedType,
        potential_subtype: &NamedType,
    ) -> Result<bool, FederationError> {
        let subtype_def = self
            .schema()
            .schema()
            .types
            .get(potential_subtype)
            .ok_or_else(|| SingleFederationError::Internal {
                message: format!("Cannot find type '{}' in schema", potential_subtype),
            })?;

        let supertype_def = self
            .schema()
            .schema()
            .types
            .get(potential_supertype)
            .ok_or_else(|| SingleFederationError::Internal {
                message: format!("Cannot find type '{}' in schema", potential_supertype),
            })?;

        // Direct subtyping relationships (interface/union) are always supported
        match (subtype_def, supertype_def) {
            // Object type implementing an interface
            (ExtendedType::Object(obj), ExtendedType::Interface(_)) => {
                Ok(obj.implements_interfaces.contains(potential_supertype))
            }
            // Interface extending another interface
            (ExtendedType::Interface(sub_intf), ExtendedType::Interface(_)) => {
                Ok(sub_intf.implements_interfaces.contains(potential_supertype))
            }
            // Object type that is a member of a union
            (ExtendedType::Object(_), ExtendedType::Union(union_type)) => {
                Ok(union_type.members.contains(potential_subtype))
            }
            // Interface that is a member of a union (if supported)
            (ExtendedType::Interface(_), ExtendedType::Union(union_type)) => {
                Ok(union_type.members.contains(potential_subtype))
            }
            _ => Ok(false),
        }
    }

    pub(crate) fn copy_type_reference(
        &mut self,
        source_type: &Type,
    ) -> Result<Type, FederationError> {
        // Check if the type is already defined in the target schema
        let source_type_cloned = source_type.clone();
        let target_schema = self.schema().schema();

        let name = source_type_cloned.inner_named_type();
        if !target_schema.types.contains_key(name) {
            self.error_reporter_mut()
                .add_error(CompositionError::InternalError {
                    message: format!("Cannot find type '{}' in target schema", name),
                });
        }

        Ok(source_type_cloned)
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
        fn enum_example_ast(&self) -> Option<EnumExampleAst> {
            Some(EnumExampleAst::Field(Node::new(FieldDefinition {
                name: Name::new("dummy").unwrap(),
                description: None,
                arguments: vec![],
                directives: Default::default(),
                ty: Type::Named(Name::new("String").unwrap()),
            })))
        }
    }

    fn create_test_schema() -> apollo_compiler::Schema {
        let mut schema = apollo_compiler::Schema::new();

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

    fn create_test_merger() -> Result<Merger, FederationError> {
        crate::merger::merge_enum::tests::create_test_merger()
    }

    #[test]
    fn same_types() {
        let schema = create_test_schema();
        let mut merger = create_test_merger().expect("Failed to create test merger");

        let mut sources: TypeSources = (0..2).map(|i| (i, None)).collect();
        sources.insert(0, Some(Type::Named(Name::new("String").unwrap())));
        sources.insert(1, Some(Type::Named(Name::new("String").unwrap())));

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
        let mut merger = create_test_merger().expect("Failed to create test merger");

        let mut sources: TypeSources = (0..2).map(|i| (i, None)).collect();
        sources.insert(0, Some(Type::NonNullNamed(Name::new("String").unwrap())));
        sources.insert(1, Some(Type::Named(Name::new("String").unwrap())));

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
        let mut merger = create_test_merger().expect("Failed to create test merger");

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
        let mut merger = create_test_merger().expect("Failed to create test merger");

        let mut sources: TypeSources = (0..2).map(|i| (i, None)).collect();
        sources.insert(0, Some(Type::Named(Name::new("I").unwrap())));
        sources.insert(1, Some(Type::Named(Name::new("A").unwrap())));

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
        let mut merger = create_test_merger().expect("Failed to create test merger");

        let mut sources: TypeSources = (0..2).map(|i| (i, None)).collect();
        sources.insert(0, Some(Type::Named(Name::new("String").unwrap())));
        sources.insert(1, Some(Type::Named(Name::new("Int").unwrap())));

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
        let mut merger = create_test_merger().expect("Failed to create test merger");

        // Test enum usage in output position
        let mut sources: TypeSources = (0..2).map(|i| (i, None)).collect();
        sources.insert(0, Some(Type::Named(Name::new("Status").unwrap())));
        sources.insert(1, Some(Type::Named(Name::new("Status").unwrap())));

        let _ = merger.merge_type_reference(
            &sources,
            &mut TestSchemaElement {
                coordinate: "user_status".to_string(),
                typ: None,
            },
            false,
        );

        // Test enum usage in input position
        let mut arg_sources: TypeSources = (0..2).map(|i| (i, None)).collect();
        arg_sources.insert(0, Some(Type::Named(Name::new("Status").unwrap())));
        arg_sources.insert(1, Some(Type::Named(Name::new("Status").unwrap())));

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
                assert_eq!(input_example.coordinate, "status_filter");
                assert_eq!(output_example.coordinate, "user_status");
            }
            _ => panic!("Expected Both usage, got {:?}", usage),
        }
    }

    #[test]
    fn enum_usage_input_only() {
        let schema = create_test_schema();
        let mut merger = create_test_merger().expect("Failed to create test merger");

        // Test enum usage only in input position
        let mut sources: TypeSources = (0..2).map(|i| (i, None)).collect();
        sources.insert(0, Some(Type::Named(Name::new("Status").unwrap())));
        sources.insert(1, Some(Type::Named(Name::new("Status").unwrap())));

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
                assert_eq!(input_example.coordinate, "status_filter");
            }
            _ => panic!("Expected Input usage, got {:?}", usage),
        }
    }

    #[test]
    fn enum_usage_output_only() {
        let schema = create_test_schema();
        let mut merger = create_test_merger().expect("Failed to create test merger");

        // Test enum usage only in output position
        let mut sources: TypeSources = (0..2).map(|i| (i, None)).collect();
        sources.insert(0, Some(Type::Named(Name::new("Status").unwrap())));
        sources.insert(1, Some(Type::Named(Name::new("Status").unwrap())));

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
                assert_eq!(output_example.coordinate, "user_status");
            }
            _ => panic!("Expected Output usage, got {:?}", usage),
        }
    }

    #[test]
    fn panics_when_no_source_has_defined_type() {
        let schema = create_test_schema();
        let mut merger = create_test_merger().expect("Failed to create test merger");

        let sources: TypeSources = (0..2).map(|i| (i, None)).collect();
        // both entries None by default

        let mut element = TestSchemaElement {
            coordinate: "f".into(),
            typ: None,
        };
        // should panic
        let result = merger.merge_type_reference(&sources, &mut element, false);
        assert!(
            result.is_ok_and(|x| !x),
            "Expected merge to fail with no defined types"
        );
    }

    #[test]
    fn field_definition_not_argument_defaults_to_output() {
        let schema = create_test_schema();
        let mut merger = create_test_merger().expect("Failed to create test merger");

        let mut sources: TypeSources = (0..1).map(|i| (i, None)).collect();
        sources.insert(0, Some(Type::Named(Name::new("A").unwrap())));

        let mut element = TestSchemaElement {
            coordinate: "f".into(),
            typ: None,
        };
        let res = merger.merge_type_reference(&sources, &mut element, false);
        assert!(
            res.is_ok(),
            "Expected merge to succeed for field definition"
        );
        assert_eq!(element.typ.unwrap().to_string(), "A");
        // enum_usages remains empty for non-enum
        assert!(merger.enum_usages().is_empty());
    }

    #[test]
    fn strict_subtype_interface_object() {
        // Object A implementing interface I => A <: I
        let schema = create_test_schema();
        let merger = create_test_merger().expect("Failed to create test merger");
        let iface = Type::Named(Name::new("I").unwrap());
        let obj = Type::Named(Name::new("A").unwrap());
        assert!(merger.is_strict_subtype(&iface, &obj).unwrap());
        assert!(!merger.is_strict_subtype(&obj, &iface).unwrap());
    }

    #[test]
    fn strict_subtype_union_membership() {
        // Member A of union U => A <: U
        let schema = create_test_schema();
        let merger = create_test_merger().expect("Failed to create test merger");
        let member = Type::Named(Name::new("A").unwrap());
        let union = Type::Named(Name::new("U").unwrap());
        assert!(merger.is_strict_subtype(&union, &member).unwrap());
    }

    #[test]
    fn skips_undefined_sources() {
        let schema = create_test_schema();
        let mut merger = create_test_merger().expect("Failed to create test merger");

        // subgraph 1 and 3 define A, subgraph 2 is None
        let mut sources: TypeSources = (0..3).map(|i| (i, None)).collect();
        sources.insert(0, Some(Type::Named(Name::new("A").unwrap())));
        // index 1 left None
        sources.insert(2, Some(Type::Named(Name::new("A").unwrap())));

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
        assert_eq!(element.typ.unwrap().to_string(), "A");
    }

    #[test]
    fn interface_subtype_behavior() {
        let schema = create_test_schema();
        let mut merger = create_test_merger().expect("Failed to create test merger");

        // A implements I
        let a = Type::Named(Name::new("A").unwrap());
        let i = Type::Named(Name::new("I").unwrap());
        let mut sources: TypeSources = (0..2).map(|i| (i, None)).collect();
        sources.insert(0, Some(a.clone()));
        sources.insert(1, Some(i.clone()));

        // Output: pick subtype A
        let mut elem_out = TestSchemaElement {
            coordinate: "f".into(),
            typ: None,
        };
        let res_out = merger.merge_type_reference(&sources, &mut elem_out, false);
        assert!(res_out.is_ok() && !res_out.unwrap());
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
        assert!(res_in.is_ok() && !res_in.unwrap());
        assert_eq!(elem_in.typ.unwrap().to_string(), "I");
        assert!(
            merger.has_hints(),
            "Expected a compatibility hint for deep subtype chain"
        );
    }

    #[test]
    fn list_and_interface_covariance() {
        let schema = create_test_schema();
        let mut merger = create_test_merger().expect("Failed to create test merger");

        // A implements interface I
        let a = Type::Named(Name::new("A").unwrap());
        let i = Type::Named(Name::new("I").unwrap());
        let list_a = Type::List(Box::new(a.clone()));
        let list_i = Type::List(Box::new(i.clone()));

        let mut sources: TypeSources = (0..2).map(|i| (i, None)).collect();
        sources.insert(0, Some(list_a.clone()));
        sources.insert(1, Some(list_i.clone()));

        // Output: pick subtype [A]
        let mut out = TestSchemaElement {
            coordinate: "f".into(),
            typ: None,
        };
        let res_out = merger.merge_type_reference(&sources, &mut out, false);
        assert!(res_out.is_ok() && !res_out.unwrap());
        assert_eq!(out.typ.unwrap().to_string(), "[A]");

        // Input: pick supertype [I]
        let mut inp = TestSchemaElement {
            coordinate: "arg".into(),
            typ: None,
        };
        let res_in = merger.merge_type_reference(&sources, &mut inp, true);
        assert!(res_in.is_ok() && !res_in.unwrap());
        assert_eq!(inp.typ.unwrap().to_string(), "[I]");
    }

    #[test]
    fn nested_list_covariance() {
        let schema = create_test_schema();
        let mut merger = create_test_merger().expect("Failed to create test merger");

        // Test nested lists: [[A]] vs [[I]]
        let a = Type::Named(Name::new("A").unwrap());
        let i = Type::Named(Name::new("I").unwrap());
        let list_a = Type::List(Box::new(a.clone()));
        let list_i = Type::List(Box::new(i.clone()));
        let list2_a = Type::List(Box::new(list_a.clone()));
        let list2_i = Type::List(Box::new(list_i.clone()));

        let mut sources: TypeSources = (0..2).map(|i| (i, None)).collect();
        sources.insert(0, Some(list2_a.clone()));
        sources.insert(1, Some(list2_i.clone()));

        // Output: pick subtype [[A]]
        let mut out = TestSchemaElement {
            coordinate: "f".into(),
            typ: None,
        };
        let res_out = merger.merge_type_reference(&sources, &mut out, false);
        assert!(res_out.is_ok() && !res_out.unwrap());
        assert_eq!(out.typ.unwrap().to_string(), "[[A]]");

        // Input: pick supertype [[I]]
        let mut inp = TestSchemaElement {
            coordinate: "arg".into(),
            typ: None,
        };
        let res_in = merger.merge_type_reference(&sources, &mut inp, true);
        assert!(res_in.is_ok() && !res_in.unwrap());
        assert_eq!(inp.typ.unwrap().to_string(), "[[I]]");
    }

    #[test]
    fn merge_with_field_definition_element() {
        let schema = create_test_schema();
        let mut merger = create_test_merger().expect("Failed to create test merger");

        // Prepare a field definition in the schema
        let mut field_def = FieldDefinition {
            name: Name::new("field").unwrap(),
            description: None,
            arguments: vec![],
            directives: Default::default(),
            ty: Type::Named(Name::new("String").unwrap()),
        };
        let mut sources: TypeSources = (0..1).map(|i| (i, None)).collect();
        sources.insert(0, Some(Type::Named(Name::new("String").unwrap())));

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
        let mut merger = create_test_merger().expect("Failed to create test merger");

        // Prepare an input value definition (argument) type
        let mut input_def = InputValueDefinition {
            name: Name::new("arg").unwrap(),
            description: None,
            default_value: None,
            directives: Default::default(),
            ty: Type::Named(Name::new("Int").unwrap()).into(),
        };
        let mut sources: TypeSources = (0..2).map(|i| (i, None)).collect();
        sources.insert(0, Some(Type::Named(Name::new("Int").unwrap())));
        sources.insert(1, Some(Type::NonNullNamed(Name::new("Int").unwrap())));

        // In input position, non-null should be overridden by nullable
        let res = merger.merge_type_reference(&sources, &mut input_def, true);
        assert!(
            res.is_ok() && !res.unwrap(),
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
