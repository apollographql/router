//! Authorization plugin

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

pub(crate) const AUTHENTICATED_DIRECTIVE_NAME: &str = "authenticated";

pub(crate) struct AuthenticatedCheckVisitor<'a> {
    schema: &'a schema::Schema,
    fragments: HashMap<&'a ast::Name, &'a ast::FragmentDefinition>,
    pub(crate) found: bool,
}

impl<'a> AuthenticatedCheckVisitor<'a> {
    pub(crate) fn new(schema: &'a schema::Schema, executable: &'a ast::Document) -> Self {
        Self {
            schema,
            fragments: transform::collect_fragments(executable),
            found: false,
        }
    }

    fn is_field_authenticated(&self, field: &schema::FieldDefinition) -> bool {
        field.directives.has(AUTHENTICATED_DIRECTIVE_NAME)
            || self
                .schema
                .types
                .get(field.ty.inner_named_type())
                .is_some_and(|t| self.is_type_authenticated(t))
    }

    fn is_type_authenticated(&self, t: &schema::ExtendedType) -> bool {
        t.directives().has(AUTHENTICATED_DIRECTIVE_NAME)
    }
}

impl<'a> traverse::Visitor for AuthenticatedCheckVisitor<'a> {
    fn field(
        &mut self,
        _parent_type: &str,
        field_def: &ast::FieldDefinition,
        node: &ast::Field,
    ) -> Result<(), BoxError> {
        let field_name = &node.name;
        if self.is_field_authenticated(field_def) {
            self.found = true;
            return Ok(());
        }
        traverse::field(self, field_def, node)
    }

    fn fragment_definition(&mut self, node: &ast::FragmentDefinition) -> Result<(), BoxError> {
        if self
            .schema
            .types
            .get(&node.type_condition)
            .is_some_and(|type_definition| self.is_type_authenticated(type_definition))
        {
            self.found = true;
            return Ok(());
        }
        traverse::fragment_definition(self, node)
    }

    fn fragment_spread(&mut self, node: &ast::FragmentSpread) -> Result<(), BoxError> {
        let condition = &self
            .fragments
            .get(&node.fragment_name)
            .ok_or("MissingFragment")?
            .type_condition;

        if self
            .schema
            .types
            .get(condition)
            .is_some_and(|type_definition| self.is_type_authenticated(type_definition))
        {
            self.found = true;
            return Ok(());
        }
        traverse::fragment_spread(self, node)
    }

    fn inline_fragment(
        &mut self,
        parent_type: &str,
        node: &ast::InlineFragment,
    ) -> Result<(), BoxError> {
        if let Some(name) = &node.type_condition {
            if self
                .schema
                .types
                .get(name)
                .is_some_and(|type_definition| self.is_type_authenticated(type_definition))
            {
                self.found = true;
                return Ok(());
            }
        }

        traverse::inline_fragment(self, parent_type, node)
    }

    fn schema(&self) -> &apollo_compiler::Schema {
        self.schema
    }
}

pub(crate) struct AuthenticatedVisitor<'a> {
    schema: &'a schema::Schema,
    fragments: HashMap<&'a ast::Name, &'a ast::FragmentDefinition>,
    implementers_map: &'a HashMap<Name, HashSet<Name>>,
    pub(crate) query_requires_authentication: bool,
    pub(crate) unauthorized_paths: Vec<Path>,
    current_path: Path,
}

impl<'a> AuthenticatedVisitor<'a> {
    pub(crate) fn new(
        schema: &'a schema::Schema,
        executable: &'a ast::Document,
        implementers_map: &'a HashMap<Name, HashSet<Name>>,
    ) -> Self {
        Self {
            schema,
            fragments: transform::collect_fragments(executable),
            implementers_map,
            query_requires_authentication: false,
            unauthorized_paths: Vec::new(),
            current_path: Path::default(),
        }
    }

    fn is_field_authenticated(&self, field: &schema::FieldDefinition) -> bool {
        field.directives.has(AUTHENTICATED_DIRECTIVE_NAME)
            || self
                .schema
                .types
                .get(field.ty.inner_named_type())
                .is_some_and(|t| self.is_type_authenticated(t))
    }

    fn is_type_authenticated(&self, t: &schema::ExtendedType) -> bool {
        t.directives().has(AUTHENTICATED_DIRECTIVE_NAME)
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
            let mut is_authenticated: Option<bool> = None;

            for ty in self
                .implementers_map
                .get(type_name)
                .into_iter()
                .flatten()
                .filter_map(|ty| self.schema.types.get(ty))
            {
                let ty_is_authenticated = ty.directives().has(AUTHENTICATED_DIRECTIVE_NAME);
                match is_authenticated {
                    None => is_authenticated = Some(ty_is_authenticated),
                    Some(other_ty_is_authenticated) => {
                        if ty_is_authenticated != other_ty_is_authenticated {
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
                let mut is_authenticated: Option<bool> = None;

                for ty in self.implementers_map.get(parent_type).into_iter().flatten() {
                    if let Ok(f) = self.schema.type_field(ty, &field.name) {
                        let field_is_authenticated = f.directives.has(AUTHENTICATED_DIRECTIVE_NAME);
                        match is_authenticated {
                            Some(other) => {
                                if field_is_authenticated != other {
                                    return true;
                                }
                            }
                            _ => {
                                is_authenticated = Some(field_is_authenticated);
                            }
                        }
                    }
                }
            }
        }
        false
    }
}

impl<'a> transform::Visitor for AuthenticatedVisitor<'a> {
    fn operation(
        &mut self,
        root_type: &str,
        node: &ast::OperationDefinition,
    ) -> Result<Option<ast::OperationDefinition>, BoxError> {
        let operation_requires_authentication = self
            .schema
            .get_object(root_type)
            .is_some_and(|ty| ty.directives.has(AUTHENTICATED_DIRECTIVE_NAME));

        if operation_requires_authentication {
            self.unauthorized_paths.push(self.current_path.clone());
            self.query_requires_authentication = true;
            Ok(None)
        } else {
            transform::operation(self, root_type, node)
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

        let field_requires_authentication = self.is_field_authenticated(field_def);

        self.current_path
            .push(PathElement::Key(field_name.as_str().into()));
        if is_field_list {
            self.current_path.push(PathElement::Flatten);
        }

        let implementors_with_different_requirements =
            self.implementors_with_different_requirements(field_def, node);

        let implementors_with_different_field_requirements =
            self.implementors_with_different_field_requirements(parent_type, node);

        let res = if field_requires_authentication
            || implementors_with_different_requirements
            || implementors_with_different_field_requirements
        {
            self.unauthorized_paths.push(self.current_path.clone());
            self.query_requires_authentication = true;
            Ok(None)
        } else {
            transform::field(self, field_def, node)
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
        let fragment_requires_authentication = self
            .schema
            .types
            .get(&node.type_condition)
            .is_some_and(|type_definition| self.is_type_authenticated(type_definition));

        if fragment_requires_authentication {
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

        let fragment_requires_authentication = self
            .schema
            .types
            .get(condition)
            .is_some_and(|type_definition| self.is_type_authenticated(type_definition));

        let res = if fragment_requires_authentication {
            self.query_requires_authentication = true;
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

                let fragment_requires_authentication = self
                    .schema
                    .types
                    .get(name)
                    .is_some_and(|type_definition| self.is_type_authenticated(type_definition));

                let res = if fragment_requires_authentication {
                    self.query_requires_authentication = true;
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
    use apollo_compiler::ast;
    use apollo_compiler::Schema;
    use multimap::MultiMap;
    use serde_json_bytes::json;
    use tower::ServiceExt;

    use crate::http_ext::TryIntoHeaderName;
    use crate::http_ext::TryIntoHeaderValue;
    use crate::json_ext::Path;
    use crate::plugin::test::MockSubgraph;
    use crate::plugins::authorization::authenticated::AuthenticatedVisitor;
    use crate::services::router::ClientRequestAccepts;
    use crate::services::supergraph;
    use crate::spec::query::transform;
    use crate::Context;
    use crate::MockedSubgraphs;
    use crate::TestHarness;

    static BASIC_SCHEMA: &str = r#"
    directive @authenticated on OBJECT | FIELD_DEFINITION | INTERFACE | SCALAR | ENUM
    directive @defer on INLINE_FRAGMENT | FRAGMENT_SPREAD

    type Query {
      topProducts: Product
      customer: User
      me: User @authenticated
      itf: I!
    }

    type Mutation @authenticated {
        ping: User
        other: String
    }

    interface I {
        id: ID
    }

    type Product {
      type: String
      price(setPrice: Int): Int
      reviews: [Review] @authenticated
      internal: Internal
      publicReviews: [Review]
      nonNullId: ID! @authenticated
    }

    scalar Internal @authenticated @specifiedBy(url: "http///example.com/test")

    type Review {
        body: String
        author: User
    }

    type User
        implements I
        @authenticated {
      id: ID
      name: String
    }
    "#;

    #[track_caller]
    fn filter(schema: &str, query: &str) -> (ast::Document, Vec<Path>) {
        let schema = Schema::parse(schema, "schema.graphql");
        let doc = ast::Document::parse(query, "query.graphql");
        schema.validate().unwrap();
        doc.to_executable(&schema).validate(&schema).unwrap();

        let map = schema.implementers_map();
        let mut visitor = AuthenticatedVisitor::new(&schema, &doc, &map);

        (
            transform::document(&mut visitor, &doc).unwrap(),
            visitor.unauthorized_paths,
        )
    }

    struct TestResult<'a> {
        query: &'a str,
        result: ast::Document,
        paths: Vec<Path>,
    }

    impl<'a> std::fmt::Display for TestResult<'a> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(
                f,
                "query:\n{}\nfiltered:\n{}\npaths: {:?}",
                self.query,
                self.result,
                self.paths.iter().map(|p| p.to_string()).collect::<Vec<_>>()
            )
        }
    }

    #[test]
    fn mutation() {
        static QUERY: &str = r#"
        mutation {
            other
        }
        "#;

        let (doc, paths) = filter(BASIC_SCHEMA, QUERY);

        insta::assert_display_snapshot!(TestResult {
            query: QUERY,
            result: doc,
            paths
        });
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

        let (doc, paths) = filter(BASIC_SCHEMA, QUERY);

        insta::assert_display_snapshot!(TestResult {
            query: QUERY,
            result: doc,
            paths
        });
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

        let (doc, paths) = filter(BASIC_SCHEMA, QUERY);

        insta::assert_display_snapshot!(TestResult {
            query: QUERY,
            result: doc,
            paths
        });
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

        let (doc, paths) = filter(BASIC_SCHEMA, QUERY);

        insta::assert_display_snapshot!(TestResult {
            query: QUERY,
            result: doc,
            paths
        });
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

        let (doc, paths) = filter(BASIC_SCHEMA, QUERY);

        insta::assert_display_snapshot!(TestResult {
            query: QUERY,
            result: doc,
            paths
        });
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

        let (doc, paths) = filter(BASIC_SCHEMA, QUERY);

        insta::assert_display_snapshot!(TestResult {
            query: QUERY,
            result: doc,
            paths
        });
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

        let (doc, paths) = filter(BASIC_SCHEMA, QUERY);

        insta::assert_display_snapshot!(TestResult {
            query: QUERY,
            result: doc,
            paths
        });
    }

    #[test]
    fn defer() {
        static QUERY: &str = r#"
        query {
            topProducts {
                type

                ...@defer {
                    nonNullId
                }
            }
        }
        "#;

        let (doc, paths) = filter(BASIC_SCHEMA, QUERY);

        insta::assert_display_snapshot!(TestResult {
            query: QUERY,
            result: doc,
            paths
        });
    }

    #[test]
    fn test() {
        static QUERY: &str = r#"
        query {
            topProducts {
                type
                reviews {
                    body
                }
            }

            customer {
                name
            }
        }
        "#;

        let (doc, paths) = filter(BASIC_SCHEMA, QUERY);

        insta::assert_display_snapshot!(TestResult {
            query: QUERY,
            result: doc,
            paths
        });
    }

    static INTERFACE_SCHEMA: &str = r#"
    directive @authenticated on OBJECT | FIELD_DEFINITION | INTERFACE | SCALAR | ENUM
    directive @defer on INLINE_FRAGMENT | FRAGMENT_SPREAD

    type Query {
        test: String
        itf: I!
    }

    interface I {
        id: ID
    }

    type A implements I {
        id: ID
        a: String
    }

    type B implements I @authenticated {
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

        let (doc, paths) = filter(INTERFACE_SCHEMA, QUERY);

        insta::assert_display_snapshot!(TestResult {
            query: QUERY,
            result: doc,
            paths
        });

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

        let (doc, paths) = filter(INTERFACE_SCHEMA, QUERY2);

        insta::assert_display_snapshot!(TestResult {
            query: QUERY2,
            result: doc,
            paths
        });
    }

    static INTERFACE_FIELD_SCHEMA: &str = r#"
    directive @authenticated on OBJECT | FIELD_DEFINITION | INTERFACE | SCALAR | ENUM
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
        id: ID
        other: String
        a: String
    }

    type B implements I {
        id: ID @authenticated
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

        let (doc, paths) = filter(INTERFACE_FIELD_SCHEMA, QUERY);

        insta::assert_display_snapshot!(TestResult {
            query: QUERY,
            result: doc,
            paths
        });

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

        let (doc, paths) = filter(INTERFACE_FIELD_SCHEMA, QUERY2);

        insta::assert_display_snapshot!(TestResult {
            query: QUERY2,
            result: doc,
            paths
        });
    }

    #[test]
    fn union() {
        static UNION_MEMBERS_SCHEMA: &str = r#"
        directive @authenticated on OBJECT | FIELD_DEFINITION | INTERFACE | SCALAR | ENUM
        directive @defer on INLINE_FRAGMENT | FRAGMENT_SPREAD

        type Query {
            test: String
            uni: I!
        }

        union I = A | B

        type A {
            id: ID
        }

        type B @authenticated {
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

        let (doc, paths) = filter(UNION_MEMBERS_SCHEMA, QUERY);

        insta::assert_display_snapshot!(TestResult {
            query: QUERY,
            result: doc,
            paths
        });
    }

    const SCHEMA: &str = r#"schema
        @core(feature: "https://specs.apollo.dev/core/v0.1")
        @core(feature: "https://specs.apollo.dev/join/v0.1")
        @core(feature: "https://specs.apollo.dev/inaccessible/v0.1")
         {
        query: Query
   }
   directive @core(feature: String!) repeatable on SCHEMA
   directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet) on FIELD_DEFINITION
   directive @join__type(graph: join__Graph!, key: join__FieldSet) repeatable on OBJECT | INTERFACE
   directive @join__owner(graph: join__Graph!) on OBJECT | INTERFACE
   directive @join__graph(name: String!, url: String!) on ENUM_VALUE
   directive @inaccessible on OBJECT | FIELD_DEFINITION | INTERFACE | UNION
   scalar join__FieldSet
   enum join__Graph {
       USER @join__graph(name: "user", url: "http://localhost:4001/graphql")
       ORGA @join__graph(name: "orga", url: "http://localhost:4002/graphql")
   }

   directive @authenticated on OBJECT | FIELD_DEFINITION | INTERFACE | SCALAR | ENUM

   type Query {
       currentUser: User @join__field(graph: USER) @authenticated
       orga(id: ID): Organization @join__field(graph: ORGA)
   }
   type User
   @join__owner(graph: USER)
   @join__type(graph: ORGA, key: "id")
   @join__type(graph: USER, key: "id"){
       id: ID!
       name: String
       phone: String @authenticated
       activeOrganization: Organization
   }
   type Organization
   @join__owner(graph: ORGA)
   @join__type(graph: ORGA, key: "id")
   @join__type(graph: USER, key: "id") {
       id: ID
       creatorUser: User
       name: String
       nonNullId: ID! @authenticated
       suborga: [Organization]
   }"#;

    #[tokio::test]
    async fn authenticated_request() {
        let subgraphs = MockedSubgraphs([
        ("user", MockSubgraph::builder().with_json(
                serde_json::json!{{
                    "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on User{name phone}}}",
                    "variables": {
                        "representations": [
                            { "__typename": "User", "id":0 }
                        ],
                    }
                }},
                serde_json::json! {{
                    "data": {
                        "_entities":[
                            {
                                "name":"Ada",
                                "phone": "1234"
                            }
                        ]
                    }
                }},
            ).build()),
        ("orga", MockSubgraph::builder().with_json(
            serde_json::json!{{"query":"{orga(id:1){id creatorUser{__typename id}}}"}},
            serde_json::json!{{"data": {"orga": { "id": 1, "creatorUser": { "__typename": "User", "id": 0 } }}}}
        ).build())
    ].into_iter().collect());

        let service = TestHarness::builder()
            .configuration_json(serde_json::json!({
            "include_subgraph_errors": {
                "all": true
            },
            "authorization": {
                "preview_directives": {
                    "enabled": true
                }
            }}))
            .unwrap()
            .schema(SCHEMA)
            .extra_plugin(subgraphs)
            .build_supergraph()
            .await
            .unwrap();

        let context = Context::new();
        context
            .insert(
                "apollo_authentication::JWT::claims",
                "placeholder".to_string(),
            )
            .unwrap();
        let request = supergraph::Request::fake_builder()
            .query("query { orga(id: 1) { id creatorUser { id name phone } } }")
            .variables(
                json! {{ "isAuthenticated": true }}
                    .as_object()
                    .unwrap()
                    .clone(),
            )
            .context(context)
            // Request building here
            .build()
            .unwrap();
        let response = service
            .oneshot(request)
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap();

        insta::assert_json_snapshot!(response);
    }

    #[tokio::test]
    async fn unauthenticated_request() {
        let subgraphs = MockedSubgraphs([
        ("user", MockSubgraph::builder().with_json(
                serde_json::json!{{
                    "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on User{name}}}",
                    "variables": {
                        "representations": [
                            { "__typename": "User", "id":0 }
                        ],
                    }
                }},
                serde_json::json! {{
                    "data": {
                        "_entities":[
                            {
                                "name":"Ada"
                            }
                        ]
                    }
                }},
            ).build()),
        ("orga", MockSubgraph::builder().with_json(
            serde_json::json!{{"query":"{orga(id:1){id creatorUser{__typename id}}}"}},
            serde_json::json!{{"data": {"orga": { "id": 1, "creatorUser": { "__typename": "User", "id": 0 } }}}}
        ).build())
    ].into_iter().collect());

        let service = TestHarness::builder()
            .configuration_json(serde_json::json!({
            "include_subgraph_errors": {
                "all": true
            },
            "authorization": {
                "preview_directives": {
                    "enabled": true
                }
            }}))
            .unwrap()
            .schema(SCHEMA)
            .extra_plugin(subgraphs)
            .build_supergraph()
            .await
            .unwrap();

        let context = Context::new();
        /*context
        .insert(
            "apollo_authentication::JWT::claims",
            "placeholder".to_string(),
        )
        .unwrap();*/
        let request = supergraph::Request::fake_builder()
            .query("query { orga(id: 1) { id creatorUser { id name phone } } }")
            .variables(
                json! {{ "isAuthenticated": false }}
                    .as_object()
                    .unwrap()
                    .clone(),
            )
            .context(context)
            // Request building here
            .build()
            .unwrap();
        let response = service
            .oneshot(request)
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap();

        insta::assert_json_snapshot!(response);
    }

    #[tokio::test]
    async fn unauthenticated_request_defer() {
        let subgraphs = MockedSubgraphs([
        ("orga", MockSubgraph::builder().with_json(
            serde_json::json!{{"query":"{orga(id:1){id creatorUser{id}}}"}},
            serde_json::json!{{"data": {"orga": { "id": 1, "creatorUser": { "id": 0 } }}}}
        )
        .with_json(
            serde_json::json!{{
                "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on Orga{name}}}",
                "variables": {
                    "representations": [
                        { "__typename": "Organization", "id":1 }
                    ],
                }
            }},
            serde_json::json! {{
                "data": {
                    "_entities":[
                        {
                            "name":"Orga 1"
                        }
                    ]
                }
            }},
        ).build())
    ].into_iter().collect());

        let service = TestHarness::builder()
            .configuration_json(serde_json::json!({
            "include_subgraph_errors": {
                "all": true
            },
            "authorization": {
                "preview_directives": {
                    "enabled": true
                }
            }}))
            .unwrap()
            .schema(SCHEMA)
            .extra_plugin(subgraphs)
            .build_supergraph()
            .await
            .unwrap();

        let context = Context::new();
        /*context
        .insert(
            "apollo_authentication::JWT::claims",
            "placeholder".to_string(),
        )
        .unwrap();*/
        let mut headers: MultiMap<TryIntoHeaderName, TryIntoHeaderValue> = MultiMap::new();
        headers.insert(
            "Accept".into(),
            "multipart/mixed; deferSpec=20220824".into(),
        );
        context.private_entries.lock().insert(ClientRequestAccepts {
            multipart_defer: true,
            multipart_subscription: true,
            json: true,
            wildcard: true,
        });
        let request = supergraph::Request::fake_builder()
            .query("query { orga(id: 1) { id creatorUser { id } ... @defer { nonNullId } } }")
            .variables(
                json! {{ "isAuthenticated": false }}
                    .as_object()
                    .unwrap()
                    .clone(),
            )
            .context(context)
            .build()
            .unwrap();

        let mut response = service.oneshot(request).await.unwrap();

        let first_response = response.next_response().await.unwrap();

        insta::assert_json_snapshot!(first_response);

        assert!(response.next_response().await.is_none());
    }
}
