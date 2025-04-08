use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast::Directive;
use apollo_compiler::collections::HashMap;
use apollo_compiler::schema::ExtendedType;

use super::FederationSchema;
use super::TypeDefinitionPosition;
use super::position::FieldDefinitionPosition;
use super::position::InterfaceFieldDefinitionPosition;
use super::position::InterfaceTypeDefinitionPosition;
use super::position::ObjectFieldDefinitionPosition;
use super::position::ObjectTypeDefinitionPosition;
use crate::error::FederationError;
use crate::schema::SubgraphMetadata;
use crate::schema::position::ObjectOrInterfaceFieldDirectivePosition;
use crate::subgraph::typestate::Expanded;
use crate::subgraph::typestate::Subgraph;
use crate::utils::FallibleIterator;

// TODO: How should we serialize these? Would be nice to use thiserror for templating, but these aren't really errors.
#[derive(Clone, Debug)]
enum UpgradeChange {
    ExternalOnTypeExtensionRemoval {
        field: FieldDefinitionPosition,
    },
    TypeExtensionRemoval {
        ty: ObjectTypeDefinitionPosition,
    },
    ExternalOnInterfaceRemoval {
        field: InterfaceFieldDefinitionPosition,
    },
    ExternalOnObjectTypeRemoval {
        ty: ObjectTypeDefinitionPosition,
    },
    UnusedExternalRemoval {
        field: FieldDefinitionPosition,
    },
    TypeWithOnlyUnusedExternalsRemoval {
        ty: ObjectTypeDefinitionPosition,
    },
    InactiveProvidesOrRequiresRemoval {
        removed_directive: ObjectOrInterfaceFieldDirectivePosition,
    },
    InactiveProvidesOrRequiresFieldsRemoval {
        updated_directive: ObjectOrInterfaceFieldDirectivePosition,
    },
    ShareableFieldAddition {
        field: FieldDefinitionPosition,
    },
    ShareableTypeAddition {
        ty: ObjectTypeDefinitionPosition,
        declaring_subgraphs: Vec<Name>,
    },
    KeyOnInterfaceRemoval {
        ty: InterfaceTypeDefinitionPosition,
    },
    ProvidesOrRequiresOnInterfaceFieldRemoval {
        removed_directive: ObjectOrInterfaceFieldDirectivePosition,
    },
    ProvidesOnNonCompositeRemoval {
        removed_directive: ObjectOrInterfaceFieldDirectivePosition,
        target_type: Name,
    },
    FieldsArgumentCoercionToString {
        updated_directive: ObjectOrInterfaceFieldDirectivePosition,
    },
    RemovedTagOnExternal {
        removed_directive: ObjectOrInterfaceFieldDirectivePosition,
    },
}

impl std::fmt::Display for UpgradeChange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        todo!()
    }
}

#[derive(Clone, Debug)]
struct SchemaUpgrader<'a> {
    schema: FederationSchema,
    original_subgraph: &'a Subgraph<Expanded>,
    subgraphs: &'a [&'a mut Subgraph<Expanded>],
    #[allow(unused)]
    object_type_map: &'a HashMap<Name, HashMap<String, TypeInfo>>,
}

#[derive(Clone, Debug)]
#[allow(unused)]
struct TypeInfo {
    pos: TypeDefinitionPosition,
    metadata: SubgraphMetadata,
}

#[allow(unused)]
pub(crate) fn upgrade_subgraphs_if_necessary(
    subgraphs: &[&mut Subgraph<Expanded>],
) -> Result<HashMap<Name, Vec<UpgradeChange>>, FederationError> {
    // if all subgraphs are fed 2, there is no upgrade to be done
    if subgraphs
        .iter()
        .all(|subgraph| subgraph.metadata().is_fed_2_schema())
    {
        return Ok(Default::default());
    }

    let mut object_type_map: HashMap<Name, HashMap<String, TypeInfo>> = Default::default();
    for subgraph in subgraphs.iter() {
        for pos in subgraph.schema().get_types() {
            if matches!(
                pos,
                TypeDefinitionPosition::Object(_) | TypeDefinitionPosition::Interface(_)
            ) {
                object_type_map
                    .entry(pos.type_name().clone())
                    .or_default()
                    .insert(
                        subgraph.name.clone(),
                        TypeInfo {
                            pos: pos.clone(),
                            metadata: subgraph.metadata().clone(), // TODO: Prefer not to clone
                        },
                    );
            }
        }
    }
    for subgraph in subgraphs.iter() {
        if !subgraph.schema().is_fed_2() {
            let mut upgrader = SchemaUpgrader::new(subgraph, subgraphs, &object_type_map)?;
            upgrader.upgrade()?;
        }
    }
    // TODO: Return federation_subgraphs
    todo!();
}

impl<'a> SchemaUpgrader<'a> {
    #[allow(unused)]
    fn new(
        original_subgraph: &'a Subgraph<Expanded>,
        subgraphs: &'a [&'a mut Subgraph<Expanded>],
        object_type_map: &'a HashMap<Name, HashMap<String, TypeInfo>>,
    ) -> Result<Self, FederationError> {
        Ok(SchemaUpgrader {
            schema: original_subgraph.schema().clone(), // TODO: Don't think we should be cloning here
            original_subgraph,
            subgraphs,
            object_type_map,
        })
    }

    #[allow(unused)]
    fn upgrade(&mut self) -> Result<Subgraph<Expanded>, FederationError> {
        self.pre_upgrade_validations();

        self.fix_federation_directives_arguments();

        self.remove_external_on_interface();

        self.remove_external_on_object_types();

        // Note that we remove all external on type extensions first, so we don't have to care about it later in @key, @provides and @requires.
        self.remove_external_on_type_extensions();

        self.fix_inactive_provides_and_requires();

        self.remove_type_extensions();

        self.remove_directives_on_interface();

        // Note that this rule rely on being after `removeDirectivesOnInterface` in practice (in that it doesn't check interfaces).
        self.remove_provides_on_non_composite();

        // Note that this should come _after_ all the other changes that may remove/update federation directives, since those may create unused
        // externals. Which is why this is toward  the end.
        self.remove_unused_externals();

        self.add_shareable();

        self.remove_tag_on_external();

        todo!();
    }

    fn pre_upgrade_validations(&self) {
        todo!();
    }

    fn fix_federation_directives_arguments(&self) {
        todo!();
    }

    fn remove_external_on_interface(&mut self) -> Result<(), FederationError> {
        let schema = &mut self.schema;
        let Some(metadata) = &schema.subgraph_metadata else {
            return Ok(());
        };
        let Ok(external_directive) = metadata
            .federation_spec_definition()
            .external_directive_definition(schema)
        else {
            return Ok(());
        };
        let mut to_delete: Vec<(InterfaceFieldDefinitionPosition, Node<Directive>)> = vec![];
        for (itf_name, ty) in schema.schema().types.iter() {
            let ExtendedType::Interface(itf) = ty else {
                continue;
            };
            let interface_pos = InterfaceTypeDefinitionPosition::new(itf_name.clone());
            for (field_name, field) in &itf.fields {
                let pos = interface_pos.field(field_name.clone());
                let external_directive = field
                    .node
                    .directives
                    .iter()
                    .find(|d| d.name == external_directive.name);
                if let Some(external_directive) = external_directive {
                    to_delete.push((pos, external_directive.clone()));
                }
            }
        }
        for (pos, directive) in to_delete {
            pos.remove_directive(schema, &directive);
        }
        Ok(())
    }

    fn remove_external_on_object_types(&mut self) -> Result<(), FederationError> {
        let schema = &mut self.schema;
        let Some(metadata) = &schema.subgraph_metadata else {
            return Ok(());
        };
        let Ok(external_directive) = metadata
            .federation_spec_definition()
            .external_directive_definition(schema)
        else {
            return Ok(());
        };
        let mut to_delete: Vec<(ObjectFieldDefinitionPosition, Node<Directive>)> = vec![];
        for (obj_name, ty) in schema.schema().types.iter() {
            let ExtendedType::Object(obj) = ty else {
                continue;
            };
            let object_pos = ObjectTypeDefinitionPosition::new(obj_name.clone());
            for (field_name, field) in &obj.fields {
                let pos = object_pos.field(field_name.clone());
                let external_directive = field
                    .node
                    .directives
                    .iter()
                    .find(|d| d.name == external_directive.name);
                if let Some(external_directive) = external_directive {
                    to_delete.push((pos, external_directive.clone()));
                }
            }
        }
        for (pos, directive) in to_delete {
            pos.remove_directive(schema, &directive);
        }
        Ok(())
    }

    fn remove_external_on_type_extensions(&self) {
        todo!();
    }

    fn fix_inactive_provides_and_requires(&self) {
        todo!();
    }

    fn remove_type_extensions(&self) {
        todo!();
    }

    fn remove_directives_on_interface(&mut self) -> Result<(), FederationError> {
        let schema = &mut self.schema;
        let Some(metadata) = &schema.subgraph_metadata else {
            return Ok(());
        };

        let _provides_directive = metadata
            .federation_spec_definition()
            .provides_directive_definition(schema)?;

        let _requires_directive = metadata
            .federation_spec_definition()
            .requires_directive_definition(schema)?;

        let _key_directive = metadata
            .federation_spec_definition()
            .key_directive_definition(schema)?;

        todo!();
    }

    fn remove_provides_on_non_composite(&mut self) -> Result<(), FederationError> {
        let schema = &mut self.schema;
        let Some(metadata) = &schema.subgraph_metadata else {
            return Ok(());
        };

        let provides_directive = metadata
            .federation_spec_definition()
            .provides_directive_definition(schema)?;

        #[allow(clippy::iter_overeager_cloned)] // TODO: remove this
        let references_to_remove: Vec<_> = schema
            .referencers()
            .get_directive(provides_directive.name.as_str())?
            .object_fields
            .iter()
            .cloned()
            .filter(|ref_field| {
                schema
                    .get_type(ref_field.type_name.clone())
                    .map(|t| !t.is_composite_type())
                    .unwrap_or(false)
            })
            .collect();
        for reference in &references_to_remove {
            reference.remove(schema)?;
        }
        Ok(())
    }

    fn remove_unused_externals(&self) {
        todo!();
    }

    fn add_shareable(&self) {
        todo!();
    }

    fn remove_tag_on_external(&mut self) -> Result<(), FederationError> {
        let schema = &mut self.schema;
        let applications = schema.tag_directive_applications()?;
        let mut to_delete: Vec<(FieldDefinitionPosition, Node<Directive>)> = vec![];
        if let Some(metadata) = &schema.subgraph_metadata {
            applications
                .iter()
                .try_for_each(|application| -> Result<(), FederationError> {
                    if let Ok(application) = (*application).as_ref() {
                        if let Ok(target) = FieldDefinitionPosition::try_from(application.target.clone()) {
                            if metadata
                                .external_metadata()
                                .is_external(&target)
                            {
                                let used_in_other_definitions =
                                    self.subgraphs.iter().fallible_any(
                                        |subgraph| -> Result<bool, FederationError> {
                                            if self.original_subgraph.name != subgraph.name {
                                                // check to see if the field is external in the other subgraphs
                                                if let Some(other_metadata) =
                                                    &subgraph.schema().subgraph_metadata
                                                {
                                                    if !other_metadata
                                                        .external_metadata()
                                                        .is_external(&target)
                                                    {
                                                        // at this point, we need to check to see if there is a @tag directive on the other subgraph that matches the current application
                                                        let other_applications = subgraph
                                                            .schema()
                                                            .tag_directive_applications()?;
                                                        return other_applications.iter().fallible_any(
                                                            |other_app_result| -> Result<bool, FederationError> {
                                                                if let Ok(other_tag_directive) =
                                                                    (*other_app_result).as_ref()
                                                                {
                                                                    if application.target
                                                                        == other_tag_directive.target
                                                                        && application.arguments.name
                                                                            == other_tag_directive
                                                                                .arguments
                                                                                .name
                                                                    {
                                                                        return Ok(true);
                                                                    }
                                                                }
                                                                Ok(false)
                                                            },
                                                        );
                                                    }
                                                }
                                            }
                                            Ok(false)
                                        },
                                    );
                                if used_in_other_definitions? {
                                    // remove @tag
                                    to_delete.push((
                                        target.clone(),
                                        application.directive.clone(),
                                    ));
                                }
                            }
                        }
                    }
                    Ok(())
                })?;
        }
        for (pos, directive) in to_delete {
            match pos {
                FieldDefinitionPosition::Object(target) => {
                    target.remove_directive(schema, &directive);
                }
                FieldDefinitionPosition::Interface(target) => {
                    target.remove_directive(schema, &directive);
                }
                FieldDefinitionPosition::Union(_target) => {
                    todo!();
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FEDERATION2_LINK_WITH_AUTO_EXPANDED_IMPORTS_UPGRADED: &'static str = r#"@link(url: "https://specs.apollo.dev/federation/v2.4", import: ["@key", "@requires", "@provides", "@external", "@tag", "@extends", "@shareable", "@inaccessible", "@override", "@composeDirective", "@interfaceObject"])"#;

    #[test]
    fn upgrades_complex_schema() {
        let mut s1 = Subgraph::parse(
            "s1",
            "",
            r#"
            type Query {
                products: [Product!]! @provides(fields: "upc description")
            }

            interface I @key(fields: "upc") {
                upc: ID!
                description: String @external
            }

            extend type Product implements I @key(fields: "upc") {
                upc: ID! @external
                name: String @external
                inventory: Int @requires(fields: "upc")
                description: String @external
            }

            # A type with a genuine 'graphqQL' extension, to ensure the extend don't get removed.
            type Random {
                x: Int @provides(fields: "x")
            }

            extend type Random {
                y: Int
            }
        "#,
        )
        .expect("parses schema")
        .expand_links()
        .expect("expands schema");

        // Note that no changes are really expected on that 2nd schema: it is just there to make the example not throw due to
        // then Product type extension having no "base".
        let mut s2 = Subgraph::parse(
            "s2",
            "",
            r#"
            type Product @key(fields: "upc") {
            upc: ID!
            name: String
            description: String
            }            
        "#,
        )
        .expect("parses schema")
        .expand_links()
        .expect("expands schema");

        let changes =
            upgrade_subgraphs_if_necessary(&vec![&mut s1, &mut s2]).expect("upgrades schema");
        let s1_changes: Vec<_> = changes
            .get("s1")
            .expect("s1 changes")
            .into_iter()
            .map(|c| c.to_string())
            .collect();
        assert!(changes.get("s2").is_none());

        assert!(s1_changes.contains(
            &r#"Removed @external from field "Product.upc" as it is a key of an extension type"#.to_string()
        ));

        assert!(
            s1_changes.contains(
                &r#"Switched type "Product" from an extension to a definition"#.to_string()
            )
        );

        assert!(s1_changes.contains(
            &r#"Removed @external field "Product.name" as it was not used in any @key, @provides or @requires"#.to_string()
        ));

        assert!(s1_changes.contains(
            &r#"Removed @external directive on interface type field "I.description": @external is nonsensical on interface fields"#.to_string()
        ));

        assert!(s1_changes.contains(
            &r#"Removed directive @requires(fields: "upc") on "Product.inventory": none of the fields were truly @external"#.to_string()
        ));

        assert!(s1_changes.contains(
            &r#"Updated directive @provides(fields: "upc description") on "Query.products" to @provides(fields: "description"): removed fields that were not truly @external"#.to_string()
        ));

        assert!(s1_changes.contains(
            &r#"Removed @key on interface "I": while allowed by federation 0.x, @key on interfaces were completely ignored/had no effect"#.to_string()
        ));

        assert!(s1_changes.contains(
            &r#"Removed @provides directive on field "Random.x" as it is of non-composite type "Int": while not rejected by federation 0.x, such @provide is nonsensical and was ignored"#.to_string()
        ));

        assert_eq!(
            s1.schema().schema().to_string(),
            r#"
            schema
                FEDERATION2_LINK_WITH_AUTO_EXPANDED_IMPORTS_UPGRADED
            {
                query: Query
            }

            type Query {
                products: [Product!]! @provides(fields: "description")
            }

            interface I {
                upc: ID!
                description: String
            }

            type Product implements I
                @key(fields: "upc")
            {
                upc: ID!
                inventory: Int
                description: String @external
            }

            type Random {
                x: Int
            }

            extend type Random {
                y: Int
            }
        "#
            .replace(
                "FEDERATION2_LINK_WITH_AUTO_EXPANDED_IMPORTS_UPGRADED",
                FEDERATION2_LINK_WITH_AUTO_EXPANDED_IMPORTS_UPGRADED
            )
        );
    }

    #[test]
    fn update_federation_directive_non_string_arguments() {
        let mut s = Subgraph::parse(
            "s",
            "",
            r#"
            type Query {
                a: A
            }

            type A @key(fields: id) @key(fields: ["id", "x"]) {
                id: String
                x: Int
            }  
        "#,
        )
        .expect("parses schema")
        .expand_links()
        .expect("expands schema");

        let changes = upgrade_subgraphs_if_necessary(&vec![&mut s]).expect("upgrades schema");
        let s_changes: Vec<_> = changes
            .get("s")
            .expect("s changes")
            .into_iter()
            .map(|c| c.to_string())
            .collect();

        assert_eq!(
            s_changes,
            vec![
                r#"Coerced "fields" argument for directive @key for "A" into a string: coerced from @key(fields: id) to @key(fields: "id")"#,
                r#"Coerced "fields" argument for directive @key for "A" into a string: coerced from @key(fields: ["id", "x"]) to @key(fields: "id x")"#,
            ]
        );

        assert_eq!(
            s.schema().schema().to_string(),
            r#"
            schema
                FEDERATION2_LINK_WITH_AUTO_EXPANDED_IMPORTS_UPGRADED
            {
                query: Query
            }

            type Query {
                a: A
            }

            type A @key(fields: "id") @key(fields: "id x") {
                id: String
                x: Int
            }
        "#
            .replace(
                "FEDERATION2_LINK_WITH_AUTO_EXPANDED_IMPORTS_UPGRADED",
                FEDERATION2_LINK_WITH_AUTO_EXPANDED_IMPORTS_UPGRADED
            )
        );
    }

    #[test]
    fn remove_tag_on_external_field_if_found_on_definition() {
        let mut s1 = Subgraph::parse(
            "s1",
            "",
            r#"
            type Query {
                a: A @provides(fields: "y")
            }

            type A @key(fields: "id") {
                id: String
                x: Int
                y: Int @external @tag(name: "a tag")
            }
        "#,
        )
        .expect("parses schema")
        .expand_links()
        .expect("expands schema");

        let mut s2 = Subgraph::parse(
            "s2",
            "",
            r#"
            type A @key(fields: "id") {
                id: String
                y: Int @tag(name: "a tag")
            }
        "#,
        )
        .expect("parses schema")
        .expand_links()
        .expect("expands schema");

        let changes =
            upgrade_subgraphs_if_necessary(&vec![&mut s1, &mut s2]).expect("upgrades schema");
        let s1_changes: Vec<_> = changes
            .get("s1")
            .expect("s1 changes")
            .into_iter()
            .map(|c| c.to_string())
            .collect();
        assert_eq!(
            s1_changes,
            vec![
                r#"Removed @tag(name: "a tag") application on @external "A.y" as the @tag application is on another definition"#
            ]
        );

        let type_a_in_s1 = s1.schema().schema().get_object("A").unwrap();
        let type_a_in_s2 = s2.schema().schema().get_object("A").unwrap();

        assert_eq!(type_a_in_s1.directives.get_all("tag").count(), 0);
        assert_eq!(
            type_a_in_s2
                .directives
                .get_all("tag")
                .map(|d| d.to_string())
                .collect::<Vec<_>>(),
            vec![r#"@tag(name: "a tag")"#]
        );
    }

    #[test]
    fn reject_interface_object_usage_if_not_all_subgraphs_are_fed2() {
        // Note that this test both validates the rejection of fed1 subgraph when @interfaceObject is used somewhere, but also
        // illustrate why we do so: fed1 schema can use @key on interface for backward compatibility, but it is ignored and
        // the schema upgrader removes them. Given that actual support for @key on interfaces is necesarry to make @interfaceObject
        // work, it would be really confusing to not reject the example below right away, since it "looks" like it the @key on
        // the interface in the 2nd subgraph should work, but it actually won't.

        // TODO
    }
}
