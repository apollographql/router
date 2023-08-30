//! Authorization plugin
//!
//! Implementation of the `@policy` directive:
//!
//! ```graphql
//! directive @policy(policies: [String!]!) on OBJECT | FIELD_DEFINITION | INTERFACE | SCALAR | ENUM
//! ```
use std::collections::HashSet;

use apollo_compiler::hir;
use apollo_compiler::hir::FieldDefinition;
use apollo_compiler::hir::TypeDefinition;
use apollo_compiler::hir::Value;
use apollo_compiler::ApolloCompiler;
use apollo_compiler::FileId;
use apollo_compiler::HirDatabase;
use tower::BoxError;

use crate::json_ext::Path;
use crate::json_ext::PathElement;
use crate::spec::query::transform;
use crate::spec::query::transform::get_field_type;
use crate::spec::query::traverse;

pub(crate) struct PolicyExtractionVisitor<'a> {
    compiler: &'a ApolloCompiler,
    file_id: FileId,
    pub(crate) extracted_policies: HashSet<String>,
}

pub(crate) const POLICY_DIRECTIVE_NAME: &str = "policy";

impl<'a> PolicyExtractionVisitor<'a> {
    #[allow(dead_code)]
    pub(crate) fn new(compiler: &'a ApolloCompiler, file_id: FileId) -> Self {
        Self {
            compiler,
            file_id,
            extracted_policies: HashSet::new(),
        }
    }

    fn get_policies_from_field(&mut self, field: &FieldDefinition) {
        self.extracted_policies
            .extend(policy_argument(field.directive_by_name(POLICY_DIRECTIVE_NAME)).cloned());

        if let Some(ty) = field.ty().type_def(&self.compiler.db) {
            self.get_policies_from_type(&ty)
        }
    }

    fn get_policies_from_type(&mut self, ty: &TypeDefinition) {
        self.extracted_policies
            .extend(policy_argument(ty.directive_by_name(POLICY_DIRECTIVE_NAME)).cloned());
    }
}

fn policy_argument(opt_directive: Option<&hir::Directive>) -> impl Iterator<Item = &String> {
    opt_directive
        .and_then(|directive| directive.argument_by_name("policies"))
        .and_then(|value| match value {
            Value::List { value, .. } => Some(value),
            _ => None,
        })
        .into_iter()
        .flatten()
        .filter_map(|v| match v {
            Value::String { value, .. } => Some(value),
            _ => None,
        })
}

impl<'a> traverse::Visitor for PolicyExtractionVisitor<'a> {
    fn compiler(&self) -> &ApolloCompiler {
        self.compiler
    }

    fn operation(&mut self, node: &hir::OperationDefinition) -> Result<(), BoxError> {
        if let Some(ty) = node.object_type(&self.compiler.db) {
            self.extracted_policies
                .extend(policy_argument(ty.directive_by_name(POLICY_DIRECTIVE_NAME)).cloned());
        }

        traverse::operation(self, node)
    }

    fn field(&mut self, parent_type: &str, node: &hir::Field) -> Result<(), BoxError> {
        if let Some(ty) = self
            .compiler
            .db
            .types_definitions_by_name()
            .get(parent_type)
        {
            if let Some(field) = ty.field(&self.compiler.db, node.name()) {
                self.get_policies_from_field(field);
            }
        }

        traverse::field(self, parent_type, node)
    }

    fn fragment_definition(&mut self, node: &hir::FragmentDefinition) -> Result<(), BoxError> {
        if let Some(ty) = self
            .compiler
            .db
            .types_definitions_by_name()
            .get(node.type_condition())
        {
            self.get_policies_from_type(ty);
        }
        traverse::fragment_definition(self, node)
    }

    fn fragment_spread(&mut self, node: &hir::FragmentSpread) -> Result<(), BoxError> {
        let fragments = self.compiler.db.fragments(self.file_id);
        let type_condition = fragments
            .get(node.name())
            .ok_or("MissingFragmentDefinition")?
            .type_condition();

        if let Some(ty) = self
            .compiler
            .db
            .types_definitions_by_name()
            .get(type_condition)
        {
            self.get_policies_from_type(ty);
        }
        traverse::fragment_spread(self, node)
    }

    fn inline_fragment(
        &mut self,
        parent_type: &str,

        node: &hir::InlineFragment,
    ) -> Result<(), BoxError> {
        if let Some(type_condition) = node.type_condition() {
            if let Some(ty) = self
                .compiler
                .db
                .types_definitions_by_name()
                .get(type_condition)
            {
                self.get_policies_from_type(ty);
            }
        }
        traverse::inline_fragment(self, parent_type, node)
    }
}

pub(crate) struct PolicyFilteringVisitor<'a> {
    compiler: &'a ApolloCompiler,
    file_id: FileId,
    request_policies: HashSet<String>,
    pub(crate) query_requires_policies: bool,
    pub(crate) unauthorized_paths: Vec<Path>,
    current_path: Path,
}

impl<'a> PolicyFilteringVisitor<'a> {
    pub(crate) fn new(
        compiler: &'a ApolloCompiler,
        file_id: FileId,
        successful_policies: HashSet<String>,
    ) -> Self {
        Self {
            compiler,
            file_id,
            request_policies: successful_policies,
            query_requires_policies: false,
            unauthorized_paths: vec![],
            current_path: Path::default(),
        }
    }

    fn is_field_authorized(&mut self, field: &FieldDefinition) -> bool {
        let field_policies = policy_argument(field.directive_by_name(POLICY_DIRECTIVE_NAME))
            .cloned()
            .collect::<HashSet<_>>();

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

        if let Some(ty) = field.ty().type_def(&self.compiler.db) {
            self.is_type_authorized(&ty)
        } else {
            false
        }
    }

    fn is_type_authorized(&self, ty: &TypeDefinition) -> bool {
        let type_policies = policy_argument(ty.directive_by_name(POLICY_DIRECTIVE_NAME))
            .cloned()
            .collect::<HashSet<_>>();
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
        parent_type: &str,
        node: &hir::Field,
    ) -> bool {
        // if all selections under the interface field are fragments with type conditions
        // then we don't need to check that they have the same authorization requirements
        if node.selection_set().fields().is_empty() {
            return false;
        }

        if let Some(type_definition) = get_field_type(self, parent_type, node.name())
            .and_then(|ty| self.compiler.db.find_type_definition_by_name(ty))
        {
            if self.implementors_with_different_type_requirements(&type_definition) {
                return true;
            }
        }
        false
    }

    fn implementors_with_different_type_requirements(&self, t: &TypeDefinition) -> bool {
        if t.is_interface_type_definition() {
            let mut policies: Option<Vec<String>> = None;

            for ty in self
                .compiler
                .db
                .subtype_map()
                .get(t.name())
                .into_iter()
                .flatten()
                .cloned()
                .filter_map(|ty| self.compiler.db.find_type_definition_by_name(ty))
            {
                // aggregate the list of scope sets
                // we transform to a common representation of sorted vectors because the element order
                // of hashsets is not stable
                let field_policies = ty
                    .directive_by_name(POLICY_DIRECTIVE_NAME)
                    .map(|directive| {
                        let mut v = policy_argument(Some(directive))
                            .cloned()
                            .collect::<Vec<_>>();
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
        field: &hir::Field,
    ) -> bool {
        if let Some(t) = self
            .compiler
            .db
            .find_type_definition_by_name(parent_type.to_string())
        {
            if t.is_interface_type_definition() {
                let mut policies: Option<Vec<String>> = None;

                for ty in self
                    .compiler
                    .db
                    .subtype_map()
                    .get(t.name())
                    .into_iter()
                    .flatten()
                    .cloned()
                    .filter_map(|ty| self.compiler.db.find_type_definition_by_name(ty))
                {
                    if let Some(f) = ty.field(&self.compiler.db, field.name()) {
                        // aggregate the list of scope sets
                        // we transform to a common representation of sorted vectors because the element order
                        // of hashsets is not stable
                        let field_policies = f
                            .directive_by_name(POLICY_DIRECTIVE_NAME)
                            .map(|directive| {
                                let mut v = policy_argument(Some(directive))
                                    .cloned()
                                    .collect::<Vec<_>>();
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
    fn compiler(&self) -> &ApolloCompiler {
        self.compiler
    }

    fn operation(
        &mut self,
        node: &hir::OperationDefinition,
    ) -> Result<Option<apollo_encoder::OperationDefinition>, BoxError> {
        let is_authorized = if let Some(ty) = node.object_type(&self.compiler.db) {
            match ty.directive_by_name(POLICY_DIRECTIVE_NAME) {
                None => true,
                Some(directive) => {
                    let type_policies = policy_argument(Some(directive))
                        .cloned()
                        .collect::<HashSet<_>>();
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
            transform::operation(self, node)
        } else {
            self.unauthorized_paths.push(self.current_path.clone());
            self.query_requires_policies = true;
            Ok(None)
        }
    }

    fn field(
        &mut self,
        parent_type: &str,
        node: &hir::Field,
    ) -> Result<Option<apollo_encoder::Field>, BoxError> {
        let field_name = node.name();
        let mut is_field_list = false;

        let is_authorized = self
            .compiler
            .db
            .types_definitions_by_name()
            .get(parent_type)
            .is_some_and(|def| {
                if let Some(field) = def.field(&self.compiler.db, field_name) {
                    if field.ty().is_list() {
                        is_field_list = true;
                    }
                    self.is_field_authorized(field)
                } else {
                    false
                }
            });

        let implementors_with_different_requirements =
            self.implementors_with_different_requirements(parent_type, node);

        let implementors_with_different_field_requirements =
            self.implementors_with_different_field_requirements(parent_type, node);

        self.current_path.push(PathElement::Key(field_name.into()));
        if is_field_list {
            self.current_path.push(PathElement::Flatten);
        }

        let res = if is_authorized
            && !implementors_with_different_requirements
            && !implementors_with_different_field_requirements
        {
            transform::field(self, parent_type, node)
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
        node: &hir::FragmentDefinition,
    ) -> Result<Option<apollo_encoder::FragmentDefinition>, BoxError> {
        let fragment_is_authorized = self
            .compiler
            .db
            .types_definitions_by_name()
            .get(node.type_condition())
            .is_some_and(|ty| self.is_type_authorized(ty));

        if !fragment_is_authorized {
            Ok(None)
        } else {
            transform::fragment_definition(self, node)
        }
    }

    fn fragment_spread(
        &mut self,
        node: &hir::FragmentSpread,
    ) -> Result<Option<apollo_encoder::FragmentSpread>, BoxError> {
        let fragments = self.compiler.db.fragments(self.file_id);
        let condition = fragments
            .get(node.name())
            .ok_or("MissingFragmentDefinition")?
            .type_condition();
        self.current_path
            .push(PathElement::Fragment(condition.into()));

        let fragment_is_authorized = self
            .compiler
            .db
            .types_definitions_by_name()
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

        node: &hir::InlineFragment,
    ) -> Result<Option<apollo_encoder::InlineFragment>, BoxError> {
        match node.type_condition() {
            None => {
                self.current_path.push(PathElement::Fragment(String::new()));
                let res = transform::inline_fragment(self, parent_type, node);
                self.current_path.pop();
                res
            }
            Some(name) => {
                self.current_path.push(PathElement::Fragment(name.into()));

                let fragment_is_authorized = self
                    .compiler
                    .db
                    .types_definitions_by_name()
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
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::collections::HashSet;

    use apollo_compiler::ApolloCompiler;
    use apollo_encoder::Document;

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
        let mut compiler = ApolloCompiler::new();

        let _schema_id = compiler.add_type_system(BASIC_SCHEMA, "schema.graphql");
        let id = compiler.add_executable(query, "query.graphql");

        let diagnostics = compiler.validate();
        for diagnostic in &diagnostics {
            println!("{diagnostic}");
        }
        assert!(diagnostics.is_empty());

        let mut visitor = PolicyExtractionVisitor::new(&compiler, id);
        traverse::document(&mut visitor, id).unwrap();

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
    fn filter(schema: &str, query: &str, policies: HashSet<String>) -> (Document, Vec<Path>) {
        let mut compiler = ApolloCompiler::new();

        let _schema_id = compiler.add_type_system(schema, "schema.graphql");
        let file_id = compiler.add_executable(query, "query.graphql");

        let diagnostics = compiler.validate();
        for diagnostic in &diagnostics {
            println!("{diagnostic}");
        }
        assert!(diagnostics.is_empty());

        let mut visitor = PolicyFilteringVisitor::new(&compiler, file_id, policies);
        (
            transform::document(&mut visitor, file_id).unwrap(),
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
