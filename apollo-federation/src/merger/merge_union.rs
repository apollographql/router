use apollo_compiler::Node;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::ComponentName;
use apollo_compiler::schema::UnionType;

use crate::error::FederationError;
use crate::merger::hints::HintCode;
use crate::merger::merge::Merger;
use crate::merger::merge::Sources;
use crate::schema::position::UnionTypeDefinitionPosition;

impl Merger {
    /// Merge union type from multiple subgraphs
    #[allow(dead_code)]
    pub(crate) fn merge_union(
        &mut self,
        sources: Sources<Node<UnionType>>,
        dest: &UnionTypeDefinitionPosition,
    ) -> Result<(), FederationError> {
        // Collect all union members from all sources
        for union_type in sources.values().flatten() {
            for member_name in union_type.members.iter() {
                if !dest
                    .get(self.merged.schema())?
                    .members
                    .contains(member_name)
                {
                    // Add the member type to the destination union
                    dest.insert_member(&mut self.merged, member_name.clone())?;
                }
            }
        }

        // For each member in the destination union, add join directives and check for inconsistencies
        let member_names: Vec<ComponentName> = dest
            .get(self.merged.schema())?
            .members
            .iter()
            .cloned()
            .collect();
        for member_name in member_names {
            self.add_join_union_member(&sources, dest, &member_name)?;
            self.hint_on_inconsistent_union_member(&sources, dest, &member_name);
        }

        Ok(())
    }

    /// Add @join__unionMember directive to union members
    fn add_join_union_member(
        &mut self,
        sources: &Sources<Node<UnionType>>,
        dest: &UnionTypeDefinitionPosition,
        member_name: &ComponentName,
    ) -> Result<(), FederationError> {
        // Add @join__unionMember directive for each subgraph that has this member
        for (&idx, source) in sources.iter() {
            if let Some(union_type) = source
                && union_type.members.contains(member_name)
            {
                // Get the join spec name for this subgraph
                let name_in_join_spec = self.join_spec_name(idx)?;

                let directive = self.join_spec_definition.union_member_directive(
                    &self.merged,
                    name_in_join_spec,
                    member_name.as_ref(),
                )?;

                // Apply the directive to the destination union
                dest.insert_directive(&mut self.merged, Component::new(directive))?;
            }
        }

        Ok(())
    }

    /// Generate hint for inconsistent union member across subgraphs
    fn hint_on_inconsistent_union_member(
        &mut self,
        sources: &Sources<Node<UnionType>>,
        dest: &UnionTypeDefinitionPosition,
        member_name: &ComponentName,
    ) {
        for union_type in sources.values().flatten() {
            // As soon as we find a subgraph that has the union type but not the member, we hint
            if !union_type.members.contains(member_name) {
                self.report_mismatch_hint(
                    HintCode::InconsistentUnionMember,
                    format!(
                        "Union type \"{}\" includes member type \"{}\" in some but not all defining subgraphs: ",
                        dest.type_name, member_name
                    ),
                    sources,
                    |source| {
                        if let Some(union_type) = source {
                            union_type.members.contains(member_name)
                        } else {
                            false
                        }
                    },
                );
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::Name;
    use apollo_compiler::ast::Value;
    use apollo_compiler::schema::ObjectType;

    use super::*;
    use crate::merger::merge_enum::tests::create_test_merger;
    use crate::schema::position::ObjectTypeDefinitionPosition;

    // Helper function to create a union type for testing
    fn create_union_type(name: &str, member_names: &[&str]) -> Node<UnionType> {
        let mut union_type = UnionType {
            description: None,
            name: Name::new_unchecked(name),
            directives: Default::default(),
            members: Default::default(),
            definition_origin: None,
        };

        for member_name in member_names {
            let name_value = Name::new(member_name).expect("Valid name");
            let component_name = ComponentName::from(name_value);
            union_type.members.insert(component_name);
        }

        Node::new(union_type)
    }

    // Helper function to create an object type for testing
    fn create_object_type(name: &str) -> Node<ObjectType> {
        let object_type = ObjectType {
            description: None,
            name: Name::new(name).expect("Valid name"),
            directives: Default::default(),
            fields: Default::default(),
            implements_interfaces: Default::default(),
            definition_origin: None,
        };

        Node::new(object_type)
    }

    fn insert_union_type(merger: &mut Merger, name: &str) -> Result<(), FederationError> {
        let union_pos = UnionTypeDefinitionPosition {
            type_name: Name::new(name).expect("Valid name"),
        };
        let union_type = create_union_type(name, &[]);
        union_pos.pre_insert(&mut merger.merged)?;
        union_pos.insert(&mut merger.merged, union_type)?;
        Ok(())
    }

    fn insert_object_type(merger: &mut Merger, name: &str) -> Result<(), FederationError> {
        let object_pos = ObjectTypeDefinitionPosition {
            type_name: Name::new(name).expect("Valid name"),
        };
        let object_type = create_object_type(name);
        object_pos.pre_insert(&mut merger.merged)?;
        object_pos.insert(&mut merger.merged, object_type)?;
        Ok(())
    }

    // Helper function to create UnionTypeDefinitionPosition for testing
    fn create_union_position(name: &str) -> UnionTypeDefinitionPosition {
        UnionTypeDefinitionPosition {
            type_name: Name::new(name).expect("Valid name"),
        }
    }

    #[test]
    fn test_union_type_creation() {
        let union1 = create_union_type("SearchResult", &["User", "Post"]);
        assert_eq!(union1.members.len(), 2);
        assert!(
            union1
                .members
                .contains(&ComponentName::from(Name::new("User").expect("Valid name")))
        );
        assert!(
            union1
                .members
                .contains(&ComponentName::from(Name::new("Post").expect("Valid name")))
        );
    }

    #[test]
    fn test_merge_union_combines_all_members() {
        let mut merger = create_test_merger().expect("Valid merger");

        // create types in supergraph
        insert_union_type(&mut merger, "SearchResult").expect("added SearchResult to supergraph");
        insert_object_type(&mut merger, "User").expect("added User to supergraph");
        insert_object_type(&mut merger, "Post").expect("added Post to supergraph");
        insert_object_type(&mut merger, "Comment").expect("added Comment to supergraph");
        // Create union types with different members
        let union1 = create_union_type("SearchResult", &["User", "Post"]);
        let union2 = create_union_type("SearchResult", &["User", "Comment"]);

        let sources: Sources<Node<UnionType>> =
            [(0, Some(union1)), (1, Some(union2))].into_iter().collect();

        let dest = create_union_position("SearchResult");

        let result = merger.merge_union(sources, &dest);
        assert!(result.is_ok());
        // Should contain all unique members from both sources
        let members = &dest
            .get(merger.merged.schema())
            .expect("union in supergraph")
            .members;
        assert_eq!(members.len(), 3);
        assert!(members.contains(&ComponentName::from(Name::new("User").expect("Valid name"))));
        assert!(members.contains(&ComponentName::from(Name::new("Post").expect("Valid name"))));
        assert!(members.contains(&ComponentName::from(
            Name::new("Comment").expect("Valid name")
        )));
    }

    #[test]
    fn test_merge_union_identical_members_across_subgraphs() {
        let mut merger = create_test_merger().expect("Valid merger");

        // create types in supergraph
        insert_union_type(&mut merger, "SearchResult").expect("added SearchResult to supergraph");
        insert_object_type(&mut merger, "User").expect("added User to supergraph");
        insert_object_type(&mut merger, "Post").expect("added Post to supergraph");

        // Create union types with identical members
        let union1 = create_union_type("SearchResult", &["User", "Post"]);
        let union2 = create_union_type("SearchResult", &["User", "Post"]);

        let sources: Sources<Node<UnionType>> =
            [(0, Some(union1)), (1, Some(union2))].into_iter().collect();

        let dest = create_union_position("SearchResult");

        let result = merger.merge_union(sources, &dest);

        assert!(result.is_ok());
        let members = &dest
            .get(merger.merged.schema())
            .expect("union in supergraph")
            .members;

        // Should contain both members
        assert_eq!(members.len(), 2);
        assert!(members.contains(&ComponentName::from(Name::new("User").expect("Valid name"))));
        assert!(members.contains(&ComponentName::from(Name::new("Post").expect("Valid name"))));

        // Verify that no hints were generated
        let (_errors, hints) = merger.error_reporter.into_errors_and_hints();
        assert_eq!(hints.len(), 0);
    }

    #[test]
    fn test_hint_on_inconsistent_union_member() {
        let mut merger = create_test_merger().expect("Valid merger");

        // create types in supergraph
        insert_union_type(&mut merger, "SearchResult").expect("added SearchResult to supergraph");
        insert_object_type(&mut merger, "User").expect("added User to supergraph");
        insert_object_type(&mut merger, "Post").expect("added Post to supergraph");

        // Create union types where one subgraph is missing a member
        let union1 = create_union_type("SearchResult", &["User", "Post"]);
        let union2 = create_union_type("SearchResult", &["User"]); // Missing Post

        let sources: Sources<Node<UnionType>> =
            [(0, Some(union1)), (1, Some(union2))].into_iter().collect();

        let dest = create_union_position("SearchResult");
        let result = merger.merge_union(sources, &dest);

        assert!(result.is_ok());
        // Verify that hint was generated
        let (_errors, hints) = merger.error_reporter.into_errors_and_hints();
        assert_eq!(hints.len(), 1);
        assert!(
            hints[0]
                .code()
                .contains(HintCode::InconsistentUnionMember.code())
        );
        assert!(hints[0].message.contains("Post"));
        assert!(hints[0].message.contains("SearchResult"));

        // validate join__unionMember directives
        let added_directives = dest.get_applied_directives(
            &merger.merged,
            &Name::new("join__unionMember").expect("Valid name"),
        );
        assert_eq!(added_directives.len(), 3);
        assert!(
            added_directives
                .iter()
                .any(|d| d.arguments.iter().any(|arg| arg.name == "graph"
                    && arg.value == Node::new(Value::Enum(Name::new_unchecked("SUBGRAPH1"))))
                    && d.arguments.iter().any(|arg| arg.name == "member"
                        && arg.value == Node::new(Value::String("User".to_string()))))
        );
        assert!(
            added_directives
                .iter()
                .any(|d| d.arguments.iter().any(|arg| arg.name == "graph"
                    && arg.value == Node::new(Value::Enum(Name::new_unchecked("SUBGRAPH1"))))
                    && d.arguments.iter().any(|arg| arg.name == "member"
                        && arg.value == Node::new(Value::String("Post".to_string()))))
        );
        assert!(
            added_directives
                .iter()
                .any(|d| d.arguments.iter().any(|arg| arg.name == "graph"
                    && arg.value == Node::new(Value::Enum(Name::new_unchecked("SUBGRAPH2"))))
                    && d.arguments.iter().any(|arg| arg.name == "member"
                        && arg.value == Node::new(Value::String("User".to_string()))))
        );
    }
}
