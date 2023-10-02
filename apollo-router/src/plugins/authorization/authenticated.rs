//! Authorization plugin

use apollo_compiler::hir;
use apollo_compiler::hir::FieldDefinition;
use apollo_compiler::hir::TypeDefinition;
use apollo_compiler::ApolloCompiler;
use apollo_compiler::FileId;
use apollo_compiler::HirDatabase;
use tower::BoxError;

use crate::json_ext::Path;
use crate::json_ext::PathElement;
use crate::spec::query::transform;
use crate::spec::query::transform::get_field_type;
use crate::spec::query::traverse;

pub(crate) const AUTHENTICATED_DIRECTIVE_NAME: &str = "authenticated";

pub(crate) struct AuthenticatedCheckVisitor<'a> {
    compiler: &'a ApolloCompiler,
    file_id: FileId,
    pub(crate) found: bool,
    authenticated_directive_name: String,
}

impl<'a> AuthenticatedCheckVisitor<'a> {
    pub(crate) fn new(compiler: &'a ApolloCompiler, file_id: FileId) -> Option<Self> {
        let authenticated_directive_name = if let Some(link) = compiler
            .db
            .schema()
            .directives_by_name("link")
            .filter(|link| {
                link.argument_by_name("url")
                    .and_then(|value| value.as_str())
                    == Some("https://specs.apollo.dev/authenticated/v0.1")
            })
            .next()
        {
            link.argument_by_name("as")
                .and_then(|value| value.as_str().map(|s| s.to_string()))
                .unwrap_or_else(|| AUTHENTICATED_DIRECTIVE_NAME.to_string())
        } else {
            return None;
        };

        Some(Self {
            compiler,
            file_id,
            found: false,
            authenticated_directive_name,
        })
    }

    fn is_field_authenticated(&self, field: &FieldDefinition) -> bool {
        field
            .directive_by_name(&self.authenticated_directive_name)
            .is_some()
            || field
                .ty()
                .type_def(&self.compiler.db)
                .map(|t| self.is_type_authenticated(&t))
                .unwrap_or(false)
    }

    fn is_type_authenticated(&self, t: &TypeDefinition) -> bool {
        t.directive_by_name(&self.authenticated_directive_name)
            .is_some()
    }
}

impl<'a> traverse::Visitor for AuthenticatedCheckVisitor<'a> {
    fn compiler(&self) -> &ApolloCompiler {
        self.compiler
    }

    fn operation(&mut self, node: &hir::OperationDefinition) -> Result<(), BoxError> {
        traverse::operation(self, node)
    }

    fn field(&mut self, parent_type: &str, node: &hir::Field) -> Result<(), BoxError> {
        let field_name = node.name();

        if self
            .compiler
            .db
            .types_definitions_by_name()
            .get(parent_type)
            .and_then(|def| def.field(&self.compiler.db, field_name))
            .is_some_and(|field| self.is_field_authenticated(field))
        {
            self.found = true;
            return Ok(());
        }
        traverse::field(self, parent_type, node)
    }

    fn fragment_definition(&mut self, node: &hir::FragmentDefinition) -> Result<(), BoxError> {
        if self
            .compiler
            .db
            .types_definitions_by_name()
            .get(node.type_condition())
            .is_some_and(|type_definition| self.is_type_authenticated(type_definition))
        {
            self.found = true;
            return Ok(());
        }
        traverse::fragment_definition(self, node)
    }

    fn fragment_spread(&mut self, node: &hir::FragmentSpread) -> Result<(), BoxError> {
        let fragments = self.compiler.db.fragments(self.file_id);
        let condition = fragments
            .get(node.name())
            .ok_or("MissingFragmentDefinition")?
            .type_condition();

        if self
            .compiler
            .db
            .types_definitions_by_name()
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
        node: &hir::InlineFragment,
    ) -> Result<(), BoxError> {
        if let Some(name) = node.type_condition() {
            if self
                .compiler
                .db
                .types_definitions_by_name()
                .get(name)
                .is_some_and(|type_definition| self.is_type_authenticated(type_definition))
            {
                self.found = true;
                return Ok(());
            }
        }

        traverse::inline_fragment(self, parent_type, node)
    }
}

pub(crate) struct AuthenticatedVisitor<'a> {
    compiler: &'a ApolloCompiler,
    file_id: FileId,
    pub(crate) query_requires_authentication: bool,
    pub(crate) unauthorized_paths: Vec<Path>,
    current_path: Path,
    authenticated_directive_name: String,
}

impl<'a> AuthenticatedVisitor<'a> {
    pub(crate) fn new(compiler: &'a ApolloCompiler, file_id: FileId) -> Option<Self> {
        let authenticated_directive_name = if let Some(link) = compiler
            .db
            .schema()
            .directives_by_name("link")
            .filter(|link| {
                link.argument_by_name("url")
                    .and_then(|value| value.as_str())
                    == Some("https://specs.apollo.dev/authenticated/v0.1")
            })
            .next()
        {
            link.argument_by_name("as")
                .and_then(|value| value.as_str().map(|s| s.to_string()))
                .unwrap_or_else(|| AUTHENTICATED_DIRECTIVE_NAME.to_string())
        } else {
            return None;
        };

        println!("AuthenticatedVisitor: auth directive name = {authenticated_directive_name}");

        Some(Self {
            compiler,
            file_id,
            query_requires_authentication: false,
            unauthorized_paths: Vec::new(),
            current_path: Path::default(),
            authenticated_directive_name,
        })
    }

    fn is_field_authenticated(&self, field: &FieldDefinition) -> bool {
        field
            .directive_by_name(&self.authenticated_directive_name)
            .is_some()
            || field
                .ty()
                .type_def(&self.compiler.db)
                .map(|t| self.is_type_authenticated(&t))
                .unwrap_or(false)
    }

    fn is_type_authenticated(&self, t: &TypeDefinition) -> bool {
        t.directive_by_name(&self.authenticated_directive_name)
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
            let mut is_authenticated: Option<bool> = None;

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
                let ty_is_authenticated = ty
                    .directive_by_name(&self.authenticated_directive_name)
                    .is_some();
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
        field: &hir::Field,
    ) -> bool {
        if let Some(t) = self
            .compiler
            .db
            .find_type_definition_by_name(parent_type.to_string())
        {
            if t.is_interface_type_definition() {
                let mut is_authenticated: Option<bool> = None;

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
                        let field_is_authenticated = f
                            .directive_by_name(&self.authenticated_directive_name)
                            .is_some();
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
    fn compiler(&self) -> &ApolloCompiler {
        self.compiler
    }

    fn operation(
        &mut self,
        node: &hir::OperationDefinition,
    ) -> Result<Option<apollo_encoder::OperationDefinition>, BoxError> {
        let operation_requires_authentication = node
            .object_type(&self.compiler.db)
            .map(|ty| {
                ty.directive_by_name(&self.authenticated_directive_name)
                    .is_some()
            })
            .unwrap_or(false);

        if operation_requires_authentication {
            self.unauthorized_paths.push(self.current_path.clone());
            self.query_requires_authentication = true;
            Ok(None)
        } else {
            transform::operation(self, node)
        }
    }

    fn field(
        &mut self,
        parent_type: &str,
        node: &hir::Field,
    ) -> Result<Option<apollo_encoder::Field>, BoxError> {
        let field_name = node.name();

        let mut is_field_list = false;

        let field_requires_authentication = self
            .compiler
            .db
            .types_definitions_by_name()
            .get(parent_type)
            .and_then(|def| def.field(&self.compiler.db, field_name))
            .is_some_and(|field| {
                if field.ty().is_list() {
                    is_field_list = true;
                }
                self.is_field_authenticated(field)
            });

        self.current_path.push(PathElement::Key(field_name.into()));
        if is_field_list {
            self.current_path.push(PathElement::Flatten);
        }

        let implementors_with_different_requirements =
            self.implementors_with_different_requirements(parent_type, node);

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
            transform::field(self, parent_type, node)
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
        let fragment_requires_authentication = self
            .compiler
            .db
            .types_definitions_by_name()
            .get(node.type_condition())
            .is_some_and(|type_definition| self.is_type_authenticated(type_definition));

        if fragment_requires_authentication {
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

        let fragment_requires_authentication = self
            .compiler
            .db
            .types_definitions_by_name()
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

                let fragment_requires_authentication = self
                    .compiler
                    .db
                    .types_definitions_by_name()
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
}

#[cfg(test)]
mod tests {
    use apollo_compiler::ApolloCompiler;
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

    schema
      @link(url: "https://specs.apollo.dev/link/v1.0")
      @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION)
      @link(url: "https://specs.apollo.dev/authenticated/v0.1", for: SECURITY)
    {
      query: Query
      mutation: Mutation
    }
    directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
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
    fn filter(schema: &str, query: &str) -> (apollo_encoder::Document, Vec<Path>) {
        let mut compiler = ApolloCompiler::new();

        let _schema_id = compiler.add_type_system(schema, "schema.graphql");
        let file_id = compiler.add_executable(query, "query.graphql");

        let diagnostics = compiler.validate();
        for diagnostic in &diagnostics {
            println!("{diagnostic}");
        }
        assert!(diagnostics.is_empty());

        let mut visitor = AuthenticatedVisitor::new(&compiler, file_id).unwrap();

        (
            transform::document(&mut visitor, file_id).unwrap(),
            visitor.unauthorized_paths,
        )
    }

    struct TestResult<'a> {
        query: &'a str,
        result: apollo_encoder::Document,
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
    schema
      @link(url: "https://specs.apollo.dev/link/v1.0")
      @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION)
      @link(url: "https://specs.apollo.dev/authenticated/v0.1", for: SECURITY)
    {
      query: Query
    }
    directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
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
    schema
      @link(url: "https://specs.apollo.dev/link/v1.0")
      @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION)
      @link(url: "https://specs.apollo.dev/authenticated/v0.1", for: SECURITY)
    {
      query: Query
    }
    directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
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
        schema
      @link(url: "https://specs.apollo.dev/link/v1.0")
      @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION)
      @link(url: "https://specs.apollo.dev/authenticated/v0.1", for: SECURITY)
    {
      query: Query
    }
    directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
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

    static RENAMED_SCHEMA: &str = r#"
    schema
      @link(url: "https://specs.apollo.dev/link/v1.0")
      @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION)
      @link(url: "https://specs.apollo.dev/authenticated/v0.1", as: "auth", for: SECURITY)
    {
      query: Query
      mutation: Mutation
    }
    directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
    directive @auth on OBJECT | FIELD_DEFINITION | INTERFACE | SCALAR | ENUM
    directive @defer on INLINE_FRAGMENT | FRAGMENT_SPREAD

    type Query {
      topProducts: Product
      customer: User
      me: User @auth
      itf: I!
    }

    type Mutation @auth {
        ping: User
        other: String
    }

    interface I {
        id: ID
    }

    type Product {
      type: String
      price(setPrice: Int): Int
      reviews: [Review] @auth
      internal: Internal
      publicReviews: [Review]
      nonNullId: ID! @auth
    }

    scalar Internal @auth @specifiedBy(url: "http///example.com/test")

    type Review {
        body: String
        author: User
    }

    type User
        implements I
        @auth {
      id: ID
      name: String
    }
    "#;

    #[test]
    fn renamed_directive() {
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

        let (doc, paths) = filter(RENAMED_SCHEMA, QUERY);

        insta::assert_display_snapshot!(TestResult {
            query: QUERY,
            result: doc,
            paths
        });
    }

    static ALTERNATIVE_DIRECTIVE_SCHEMA: &str = r#"
    schema
      @link(url: "https://specs.apollo.dev/link/v1.0")
      @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION)
      @link(url: "https://specs.apollo.dev/OtherAuthenticated/v0.1", import: ["@authenticated"])
    {
      query: Query
      mutation: Mutation
    }
    directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
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

    // a directive named `@authenticated` imported from a different spec should not be considered
    #[test]
    #[should_panic]
    fn alternative_directive() {
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

        let _ = filter(ALTERNATIVE_DIRECTIVE_SCHEMA, QUERY);
    }

    const SCHEMA: &str = r#"schema
      @link(url: "https://specs.apollo.dev/link/v1.0")
      @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION)
      @link(url: "https://specs.apollo.dev/authenticated/v0.1", for: SECURITY)
         {
        query: Query
   }
   directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
   directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE
   directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION
   directive @join__graph(name: String!, url: String!) on ENUM_VALUE
   directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE
   directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR
   directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION

   scalar link__Import

   enum link__Purpose {
    """
    `SECURITY` features provide metadata necessary to securely resolve fields.
    """
    SECURITY
  
    """
    `EXECUTION` features provide metadata necessary for operation execution.
    """
    EXECUTION
  }

  
   scalar join__FieldSet
   enum join__Graph {
       USER @join__graph(name: "user", url: "http://localhost:4001/graphql")
       ORGA @join__graph(name: "orga", url: "http://localhost:4002/graphql")
   }

   directive @authenticated on OBJECT | FIELD_DEFINITION | INTERFACE | SCALAR | ENUM

   type Query
   @join__type(graph: ORGA)
   @join__type(graph: USER)
   {
       currentUser: User @join__field(graph: USER) @authenticated
       orga(id: ID): Organization @join__field(graph: ORGA)
   }
   type User
   @join__type(graph: ORGA, key: "id")
   @join__type(graph: USER, key: "id"){
       id: ID!
       name: String
       phone: String @authenticated
       activeOrganization: Organization
   }
   type Organization
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
        ).with_json(
            serde_json::json!{{"query":"{orga(id:1){id creatorUser{id name phone}}}"}},
            serde_json::json!{{"data": {"orga": { "id": 1, "creatorUser": { "__typename": "User", "id": 0, "name":"Ada", "phone": "1234" } }}}}
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
            ).with_json(
                serde_json::json!{{"query":"{orga(id:1){id creatorUser{id name}}}"}},
                serde_json::json!{{"data": {"orga": { "id": 1, "creatorUser": {"id": 0, "name":"Ada" } }}}}
            ).build()),
        ("orga", MockSubgraph::builder().with_json(
            serde_json::json!{{"query":"{orga(id:1){id creatorUser{__typename id}}}"}},
            serde_json::json!{{"data": {"orga": { "id": 1, "creatorUser": { "__typename": "User", "id": 0 } }}}}
        ).with_json(
            serde_json::json!{{"query":"{orga(id:1){id creatorUser{id name}}}"}},
            serde_json::json!{{"data": {"orga": { "id": 1, "creatorUser": {"id": 0, "name":"Ada" } }}}}
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
