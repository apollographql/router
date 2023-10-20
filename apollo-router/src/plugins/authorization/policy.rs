//! Authorization plugin
//!
//! Implementation of the `@policy` directive:
//!
//! ```graphql
//! directive @policy(policies: [String!]!) on OBJECT | FIELD_DEFINITION | INTERFACE | SCALAR | ENUM
//! ```
use std::collections::HashMap;
use std::collections::HashSet;

use apollo_compiler::ast;
use apollo_compiler::schema;
use apollo_compiler::schema::Name;
use tower::BoxError;

use crate::json_ext::Path;
use crate::json_ext::PathElement;
use crate::spec::query::transform;
use crate::spec::query::traverse;

pub(crate) struct PolicyExtractionVisitor<'a> {
    schema: &'a schema::Schema,
    fragments: HashMap<&'a ast::Name, &'a ast::FragmentDefinition>,
    pub(crate) extracted_policies: HashSet<String>,
}

pub(crate) const POLICY_DIRECTIVE_NAME: &str = "policy";

impl<'a> PolicyExtractionVisitor<'a> {
    #[allow(dead_code)]
    pub(crate) fn new(schema: &'a schema::Schema, executable: &'a ast::Document) -> Self {
        Self {
            schema,
            fragments: transform::collect_fragments(executable),
            extracted_policies: HashSet::new(),
        }
    }

    fn get_policies_from_field(&mut self, field: &schema::FieldDefinition) {
        self.extracted_policies
            .extend(policy_argument(field.directives.get(POLICY_DIRECTIVE_NAME)));

        if let Some(ty) = self.schema.types.get(field.ty.inner_named_type()) {
            self.get_policies_from_type(ty)
        }
    }

    fn get_policies_from_type(&mut self, ty: &schema::ExtendedType) {
        self.extracted_policies
            .extend(policy_argument(ty.directives().get(POLICY_DIRECTIVE_NAME)));
    }
}

fn policy_argument(
    opt_directive: Option<&impl AsRef<ast::Directive>>,
) -> impl Iterator<Item = String> + '_ {
    opt_directive
        .and_then(|directive| directive.as_ref().argument_by_name("policies"))
        .and_then(|value| value.as_list())
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_owned))
}

impl<'a> traverse::Visitor for PolicyExtractionVisitor<'a> {
    fn operation(
        &mut self,
        root_type: &str,
        node: &ast::OperationDefinition,
    ) -> Result<(), BoxError> {
        if let Some(ty) = self.schema.types.get(root_type) {
            self.extracted_policies
                .extend(policy_argument(ty.directives().get(POLICY_DIRECTIVE_NAME)));
        }

        traverse::operation(self, root_type, node)
    }

    fn field(
        &mut self,
        _parent_type: &str,
        field_def: &ast::FieldDefinition,
        node: &ast::Field,
    ) -> Result<(), BoxError> {
        self.get_policies_from_field(field_def);

        traverse::field(self, field_def, node)
    }

    fn fragment_definition(&mut self, node: &ast::FragmentDefinition) -> Result<(), BoxError> {
        if let Some(ty) = self.schema.types.get(&node.type_condition) {
            self.get_policies_from_type(ty);
        }
        traverse::fragment_definition(self, node)
    }

    fn fragment_spread(&mut self, node: &ast::FragmentSpread) -> Result<(), BoxError> {
        let type_condition = &self
            .fragments
            .get(&node.fragment_name)
            .ok_or("MissingFragment")?
            .type_condition;

        if let Some(ty) = self.schema.types.get(type_condition) {
            self.get_policies_from_type(ty);
        }
        traverse::fragment_spread(self, node)
    }

    fn inline_fragment(
        &mut self,
        parent_type: &str,
        node: &ast::InlineFragment,
    ) -> Result<(), BoxError> {
        if let Some(type_condition) = &node.type_condition {
            if let Some(ty) = self.schema.types.get(type_condition) {
                self.get_policies_from_type(ty);
            }
        }
        traverse::inline_fragment(self, parent_type, node)
    }

    fn schema(&self) -> &apollo_compiler::Schema {
        self.schema
    }
}

pub(crate) struct PolicyFilteringVisitor<'a> {
    schema: &'a schema::Schema,
    fragments: HashMap<&'a ast::Name, &'a ast::FragmentDefinition>,
    implementers_map: &'a HashMap<Name, HashSet<Name>>,
    request_policies: HashSet<String>,
    pub(crate) query_requires_policies: bool,
    pub(crate) unauthorized_paths: Vec<Path>,
    current_path: Path,
}

impl<'a> PolicyFilteringVisitor<'a> {
    pub(crate) fn new(
        schema: &'a schema::Schema,
        executable: &'a ast::Document,
        implementers_map: &'a HashMap<Name, HashSet<Name>>,
        successful_policies: HashSet<String>,
    ) -> Self {
        Self {
            schema,
            fragments: transform::collect_fragments(executable),
            implementers_map,
            request_policies: successful_policies,
            query_requires_policies: false,
            unauthorized_paths: vec![],
            current_path: Path::default(),
        }
    }

    fn is_field_authorized(&mut self, field: &schema::FieldDefinition) -> bool {
        let field_policies =
            policy_argument(field.directives.get(POLICY_DIRECTIVE_NAME)).collect::<HashSet<_>>();

        // The field is authorized if any of the policies succeeds
        if !field_policies.is_empty()
            && self
                .request_policies
                .intersection(&field_policies)
                .next()
                .is_none()
        {
            return false;
        }

        if let Some(ty) = self.schema.types.get(field.ty.inner_named_type()) {
            self.is_type_authorized(ty)
        } else {
            false
        }
    }

    fn is_type_authorized(&self, ty: &schema::ExtendedType) -> bool {
        let type_policies =
            policy_argument(ty.directives().get(POLICY_DIRECTIVE_NAME)).collect::<HashSet<_>>();
        // The field is authorized if any of the policies succeeds
        type_policies.is_empty()
            || self
                .request_policies
                .intersection(&type_policies)
                .next()
                .is_some()
    }

    fn implementors_with_different_requirements(
        &self,
        field_def: &ast::FieldDefinition,
        node: &ast::Field,
    ) -> bool {
        // if all selections under the interface field are fragments with type conditions
        // then we don't need to check that they have the same authorization requirements
        if node.selection_set.iter().all(|sel| {
            matches!(
                sel,
                ast::Selection::FragmentSpread(_) | ast::Selection::InlineFragment(_)
            )
        }) {
            return false;
        }

        let type_name = field_def.ty.inner_named_type();
        if let Some(type_definition) = self.schema.types.get(type_name) {
            if self.implementors_with_different_type_requirements(type_name, type_definition) {
                return true;
            }
        }
        false
    }

    fn implementors_with_different_type_requirements(
        &self,
        type_name: &str,
        t: &schema::ExtendedType,
    ) -> bool {
        if t.is_interface() {
            let mut policies: Option<Vec<String>> = None;

            for ty in self
                .implementers_map
                .get(type_name)
                .into_iter()
                .flatten()
                .filter_map(|ty| self.schema.types.get(ty))
            {
                // aggregate the list of scope sets
                // we transform to a common representation of sorted vectors because the element order
                // of hashsets is not stable
                let field_policies = ty
                    .directives()
                    .get(POLICY_DIRECTIVE_NAME)
                    .map(|directive| {
                        let mut v = policy_argument(Some(directive)).collect::<Vec<_>>();
                        v.sort();
                        v
                    })
                    .unwrap_or_default();

                match &policies {
                    None => policies = Some(field_policies),
                    Some(other_policies) => {
                        if field_policies != *other_policies {
                            return true;
                        }
                    }
                }
            }
        }

        false
    }

    fn implementors_with_different_field_requirements(
        &self,
        parent_type: &str,
        field: &ast::Field,
    ) -> bool {
        if let Some(t) = self.schema.types.get(parent_type) {
            if t.is_interface() {
                let mut policies: Option<Vec<String>> = None;

                for ty in self.implementers_map.get(parent_type).into_iter().flatten() {
                    if let Ok(f) = self.schema.type_field(ty, &field.name) {
                        // aggregate the list of scope sets
                        // we transform to a common representation of sorted vectors because the element order
                        // of hashsets is not stable
                        let field_policies = f
                            .directives
                            .get(POLICY_DIRECTIVE_NAME)
                            .map(|directive| {
                                let mut v = policy_argument(Some(directive)).collect::<Vec<_>>();
                                v.sort();
                                v
                            })
                            .unwrap_or_default();

                        match &policies {
                            None => policies = Some(field_policies),
                            Some(other_policies) => {
                                if field_policies != *other_policies {
                                    return true;
                                }
                            }
                        }
                    }
                }
            }
        }
        false
    }
}

impl<'a> transform::Visitor for PolicyFilteringVisitor<'a> {
    fn operation(
        &mut self,
        root_type: &str,
        node: &ast::OperationDefinition,
    ) -> Result<Option<ast::OperationDefinition>, BoxError> {
        let is_authorized = if let Some(ty) = self.schema.get_object(root_type) {
            match ty.directives.get(POLICY_DIRECTIVE_NAME) {
                None => true,
                Some(directive) => {
                    let type_policies = policy_argument(Some(directive)).collect::<HashSet<_>>();
                    // The field is authorized if any of the policies succeeds
                    type_policies.is_empty()
                        || self
                            .request_policies
                            .intersection(&type_policies)
                            .next()
                            .is_some()
                }
            }
        } else {
            false
        };

        if is_authorized {
            transform::operation(self, root_type, node)
        } else {
            self.unauthorized_paths.push(self.current_path.clone());
            self.query_requires_policies = true;
            Ok(None)
        }
    }

    fn field(
        &mut self,
        parent_type: &str,
        field_def: &ast::FieldDefinition,
        node: &ast::Field,
    ) -> Result<Option<ast::Field>, BoxError> {
        let field_name = &node.name;
        let is_field_list = field_def.ty.is_list();

        let is_authorized = self.is_field_authorized(field_def);

        let implementors_with_different_requirements =
            self.implementors_with_different_requirements(field_def, node);

        let implementors_with_different_field_requirements =
            self.implementors_with_different_field_requirements(parent_type, node);

        self.current_path
            .push(PathElement::Key(field_name.as_str().into()));
        if is_field_list {
            self.current_path.push(PathElement::Flatten);
        }

        let res = if is_authorized
            && !implementors_with_different_requirements
            && !implementors_with_different_field_requirements
        {
            transform::field(self, field_def, node)
        } else {
            self.unauthorized_paths.push(self.current_path.clone());
            self.query_requires_policies = true;
            Ok(None)
        };

        if is_field_list {
            self.current_path.pop();
        }
        self.current_path.pop();

        res
    }

    fn fragment_definition(
        &mut self,
        node: &ast::FragmentDefinition,
    ) -> Result<Option<ast::FragmentDefinition>, BoxError> {
        let fragment_is_authorized = self
            .schema
            .types
            .get(&node.type_condition)
            .is_some_and(|ty| self.is_type_authorized(ty));

        if !fragment_is_authorized {
            Ok(None)
        } else {
            transform::fragment_definition(self, node)
        }
    }

    fn fragment_spread(
        &mut self,
        node: &ast::FragmentSpread,
    ) -> Result<Option<ast::FragmentSpread>, BoxError> {
        let condition = &self
            .fragments
            .get(&node.fragment_name)
            .ok_or("MissingFragment")?
            .type_condition;
        self.current_path
            .push(PathElement::Fragment(condition.as_str().into()));

        let fragment_is_authorized = self
            .schema
            .types
            .get(condition)
            .is_some_and(|ty| self.is_type_authorized(ty));

        let res = if !fragment_is_authorized {
            self.query_requires_policies = true;
            self.unauthorized_paths.push(self.current_path.clone());

            Ok(None)
        } else {
            transform::fragment_spread(self, node)
        };

        self.current_path.pop();
        res
    }

    fn inline_fragment(
        &mut self,
        parent_type: &str,
        node: &ast::InlineFragment,
    ) -> Result<Option<ast::InlineFragment>, BoxError> {
        match &node.type_condition {
            None => {
                self.current_path.push(PathElement::Fragment(String::new()));
                let res = transform::inline_fragment(self, parent_type, node);
                self.current_path.pop();
                res
            }
            Some(name) => {
                self.current_path
                    .push(PathElement::Fragment(name.as_str().into()));

                let fragment_is_authorized = self
                    .schema
                    .types
                    .get(name)
                    .is_some_and(|ty| self.is_type_authorized(ty));

                let res = if !fragment_is_authorized {
                    self.query_requires_policies = true;
                    self.unauthorized_paths.push(self.current_path.clone());
                    Ok(None)
                } else {
                    transform::inline_fragment(self, parent_type, node)
                };

                self.current_path.pop();

                res
            }
        }
    }

    fn schema(&self) -> &apollo_compiler::Schema {
        self.schema
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::collections::HashSet;

    use apollo_compiler::ast;
    use apollo_compiler::Schema;

    use crate::json_ext::Path;
    use crate::plugins::authorization::policy::PolicyExtractionVisitor;
    use crate::plugins::authorization::policy::PolicyFilteringVisitor;
    use crate::spec::query::transform;
    use crate::spec::query::traverse;

    static BASIC_SCHEMA: &str = r#"
    directive @policy(policies: [String]) on OBJECT | FIELD_DEFINITION | INTERFACE | SCALAR | ENUM

    type Query {
      topProducts: Product
      customer: User
      me: User @policy(policies: ["profile"])
      itf: I
    }

    type Mutation @policy(policies: ["mut"]) {
        ping: User @policy(policies: ["ping"])
        other: String
    }

    interface I {
        id: ID
    }

    type Product {
      type: String
      price(setPrice: Int): Int
      reviews: [Review]
      internal: Internal
      publicReviews: [Review]
    }

    scalar Internal @policy(policies: ["internal"]) @specifiedBy(url: "http///example.com/test")

    type Review @policy(policies: ["review"]) {
        body: String
        author: User
    }

    type User implements I @policy(policies: ["read user"]) {
      id: ID
      name: String @policy(policies: ["read username"])
    }
    "#;

    fn extract(query: &str) -> BTreeSet<String> {
        let schema = Schema::parse(BASIC_SCHEMA, "schema.graphql");
        let doc = ast::Document::parse(query, "query.graphql");
        schema.validate().unwrap();
        doc.to_executable(&schema).validate(&schema).unwrap();
        let mut visitor = PolicyExtractionVisitor::new(&schema, &doc);
        traverse::document(&mut visitor, &doc).unwrap();

        visitor.extracted_policies.into_iter().collect()
    }

    #[test]
    fn extract_policies() {
        static QUERY: &str = r#"
        {
            topProducts {
                type
                internal
            }

            me {
                name
            }
        }
        "#;

        let doc = extract(QUERY);
        insta::assert_debug_snapshot!(doc);
    }

    #[track_caller]
    fn filter(schema: &str, query: &str, policies: HashSet<String>) -> (ast::Document, Vec<Path>) {
        let schema = Schema::parse(schema, "schema.graphql");
        let doc = ast::Document::parse(query, "query.graphql");
        schema.validate().unwrap();
        doc.to_executable(&schema).validate(&schema).unwrap();
        let map = schema.implementers_map();
        let mut visitor = PolicyFilteringVisitor::new(&schema, &doc, &map, policies);
        (
            transform::document(&mut visitor, &doc).unwrap(),
            visitor.unauthorized_paths,
        )
    }

    #[test]
    fn filter_basic_query() {
        static QUERY: &str = r#"
        {
            topProducts {
                type
                internal
            }

            me {
                id
                name
            }
        }
        "#;

        let doc = extract(QUERY);
        insta::assert_debug_snapshot!(doc);

        let (doc, paths) = filter(BASIC_SCHEMA, QUERY, HashSet::new());
        insta::assert_display_snapshot!(doc);
        insta::assert_debug_snapshot!(paths);

        let (doc, paths) = filter(
            BASIC_SCHEMA,
            QUERY,
            ["profile".to_string(), "internal".to_string()]
                .into_iter()
                .collect(),
        );
        insta::assert_display_snapshot!(doc);
        insta::assert_debug_snapshot!(paths);

        let (doc, paths) = filter(
            BASIC_SCHEMA,
            QUERY,
            [
                "profile".to_string(),
                "read user".to_string(),
                "internal".to_string(),
            ]
            .into_iter()
            .collect(),
        );
        insta::assert_display_snapshot!(doc);
        insta::assert_debug_snapshot!(paths);

        let (doc, paths) = filter(
            BASIC_SCHEMA,
            QUERY,
            [
                "profile".to_string(),
                "read user".to_string(),
                "read username".to_string(),
            ]
            .into_iter()
            .collect(),
        );
        insta::assert_display_snapshot!(doc);
        insta::assert_debug_snapshot!(paths);
    }

    #[test]
    fn mutation() {
        static QUERY: &str = r#"
        mutation {
            ping {
                name
            }
            other
        }
        "#;

        let doc = extract(QUERY);
        insta::assert_debug_snapshot!(doc);

        let (doc, paths) = filter(BASIC_SCHEMA, QUERY, HashSet::new());

        insta::assert_display_snapshot!(doc);
        insta::assert_debug_snapshot!(paths);
    }

    #[test]
    fn query_field() {
        static QUERY: &str = r#"
        query {
            topProducts {
                type
            }

            me {
                name
            }
        }
        "#;

        let doc = extract(QUERY);
        insta::assert_debug_snapshot!(doc);

        let (doc, paths) = filter(BASIC_SCHEMA, QUERY, HashSet::new());

        insta::assert_display_snapshot!(doc);
        insta::assert_debug_snapshot!(paths);
    }

    #[test]
    fn query_field_alias() {
        static QUERY: &str = r#"
        query {
            topProducts {
                type
            }

            moi: me {
                name
            }
        }
        "#;

        let doc = extract(QUERY);
        insta::assert_debug_snapshot!(doc);

        let (doc, paths) = filter(BASIC_SCHEMA, QUERY, HashSet::new());

        insta::assert_display_snapshot!(doc);
        insta::assert_debug_snapshot!(paths);
    }

    #[test]
    fn scalar() {
        static QUERY: &str = r#"
        query {
            topProducts {
                type
                internal
            }
        }
        "#;

        let doc = extract(QUERY);
        insta::assert_debug_snapshot!(doc);

        let (doc, paths) = filter(BASIC_SCHEMA, QUERY, HashSet::new());

        insta::assert_display_snapshot!(doc);
        insta::assert_debug_snapshot!(paths);
    }

    #[test]
    fn array() {
        static QUERY: &str = r#"
        query {
            topProducts {
                type
                publicReviews {
                    body
                    author {
                        name
                    }
                }
            }
        }
        "#;

        let doc = extract(QUERY);
        insta::assert_debug_snapshot!(doc);

        let (doc, paths) = filter(BASIC_SCHEMA, QUERY, HashSet::new());

        insta::assert_display_snapshot!(doc);
        insta::assert_debug_snapshot!(paths);
    }

    #[test]
    fn interface_inline_fragment() {
        static QUERY: &str = r#"
        query {
            topProducts {
                type
            }
            itf {
                id
                ... on User {
                    name
                }
            }
        }
        "#;

        let doc = extract(QUERY);
        insta::assert_debug_snapshot!(doc);

        let (doc, paths) = filter(BASIC_SCHEMA, QUERY, HashSet::new());

        insta::assert_display_snapshot!(doc);
        insta::assert_debug_snapshot!(paths);
    }

    #[test]
    fn interface_fragment() {
        static QUERY: &str = r#"
        query {
            topProducts {
                type
            }
            itf {
                id
                ...F
            }
        }

        fragment F on User {
            name
        }
        "#;

        let doc = extract(QUERY);
        insta::assert_debug_snapshot!(doc);

        let (doc, paths) = filter(BASIC_SCHEMA, QUERY, HashSet::new());

        insta::assert_display_snapshot!(doc);
        insta::assert_debug_snapshot!(paths);

        let (doc, paths) = filter(
            BASIC_SCHEMA,
            QUERY,
            ["read user".to_string(), "read username".to_string()]
                .into_iter()
                .collect(),
        );

        insta::assert_display_snapshot!(doc);
        insta::assert_debug_snapshot!(paths);
    }

    static INTERFACE_SCHEMA: &str = r#"
    directive @policy(policies: [String]) on OBJECT | FIELD_DEFINITION | INTERFACE | SCALAR | ENUM
    directive @defer on INLINE_FRAGMENT | FRAGMENT_SPREAD
    type Query {
        test: String
        itf: I!
    }
    interface I @policy(policies: ["itf"]) {
        id: ID
    }
    type A implements I @policy(policies: ["a"]) {
        id: ID
        a: String
    }
    type B implements I @policy(policies: ["b"]) {
        id: ID
        b: String
    }
    "#;

    #[test]
    fn interface_type() {
        static QUERY: &str = r#"
        query {
            test
            itf {
                id
            }
        }
        "#;

        let (doc, paths) = filter(INTERFACE_SCHEMA, QUERY, HashSet::new());

        insta::assert_display_snapshot!(doc);
        insta::assert_debug_snapshot!(paths);

        let (doc, paths) = filter(
            INTERFACE_SCHEMA,
            QUERY,
            ["itf".to_string()].into_iter().collect(),
        );

        insta::assert_display_snapshot!(doc);
        insta::assert_debug_snapshot!(paths);

        static QUERY2: &str = r#"
        query {
            test
            itf {
                ... on A {
                    id
                }
                ... on B {
                    id
                }
            }
        }
        "#;

        let (doc, paths) = filter(INTERFACE_SCHEMA, QUERY2, HashSet::new());

        insta::assert_display_snapshot!(doc);
        insta::assert_debug_snapshot!(paths);

        let (doc, paths) = filter(
            INTERFACE_SCHEMA,
            QUERY2,
            ["itf".to_string()].into_iter().collect(),
        );

        insta::assert_display_snapshot!(doc);
        insta::assert_debug_snapshot!(paths);

        let (doc, paths) = filter(
            INTERFACE_SCHEMA,
            QUERY2,
            ["itf".to_string(), "a".to_string()].into_iter().collect(),
        );

        insta::assert_display_snapshot!(doc);
        insta::assert_debug_snapshot!(paths);
    }

    static INTERFACE_FIELD_SCHEMA: &str = r#"
    directive @policy(policies: [String]) on OBJECT | FIELD_DEFINITION | INTERFACE | SCALAR | ENUM
    directive @defer on INLINE_FRAGMENT | FRAGMENT_SPREAD
    type Query {
        test: String
        itf: I!
    }
    interface I {
        id: ID
        other: String
    }
    type A implements I {
        id: ID @policy(policies: ["a"])
        other: String
        a: String
    }
    type B implements I {
        id: ID @policy(policies: ["b"])
        other: String
        b: String
    }
    "#;

    #[test]
    fn interface_field() {
        static QUERY: &str = r#"
        query {
            test
            itf {
                id
                other
            }
        }
        "#;

        let (doc, paths) = filter(INTERFACE_FIELD_SCHEMA, QUERY, HashSet::new());

        insta::assert_display_snapshot!(doc);
        insta::assert_debug_snapshot!(paths);

        static QUERY2: &str = r#"
        query {
            test
            itf {
                ... on A {
                    id
                    other
                }
                ... on B {
                    id
                    other
                }
            }
        }
        "#;

        let (doc, paths) = filter(INTERFACE_FIELD_SCHEMA, QUERY2, HashSet::new());

        insta::assert_display_snapshot!(doc);
        insta::assert_debug_snapshot!(paths);
    }

    #[test]
    fn union() {
        static UNION_MEMBERS_SCHEMA: &str = r#"
        directive @policy(policies: [String]) on OBJECT | FIELD_DEFINITION | INTERFACE | SCALAR | ENUM
        directive @defer on INLINE_FRAGMENT | FRAGMENT_SPREAD
        type Query {
            test: String
            uni: I!
        }
        union I = A | B
        type A @policy(policies: ["a"]) {
            id: ID
        }
        type B @policy(policies: ["b"]) {
            id: ID
        }
        "#;

        static QUERY: &str = r#"
        query {
            test
            uni {
                ... on A {
                    id
                }
                ... on B {
                    id
                }
            }
        }
        "#;

        let (doc, paths) = filter(
            UNION_MEMBERS_SCHEMA,
            QUERY,
            ["a".to_string()].into_iter().collect(),
        );

        insta::assert_display_snapshot!(doc);
        insta::assert_debug_snapshot!(paths);
    }
}
