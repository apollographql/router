//! Authorization plugin

use std::collections::HashMap;

use apollo_compiler::ast;
use apollo_compiler::executable;
use apollo_compiler::schema;
use apollo_compiler::schema::Implementers;
use apollo_compiler::schema::Name;
use apollo_compiler::Node;
use tower::BoxError;

use crate::json_ext::Path;
use crate::json_ext::PathElement;
use crate::spec::query::transform;
use crate::spec::query::traverse;
use crate::spec::Schema;
use crate::spec::TYPENAME;

pub(crate) const AUTHENTICATED_DIRECTIVE_NAME: &str = "authenticated";
pub(crate) const AUTHENTICATED_SPEC_BASE_URL: &str = "https://specs.apollo.dev/authenticated";
pub(crate) const AUTHENTICATED_SPEC_VERSION_RANGE: &str = ">=0.1.0, <=0.1.0";

pub(crate) struct AuthenticatedCheckVisitor<'a> {
    schema: &'a schema::Schema,
    fragments: HashMap<&'a ast::Name, &'a Node<executable::Fragment>>,
    pub(crate) found: bool,
    authenticated_directive_name: String,
    entity_query: bool,
}

impl<'a> AuthenticatedCheckVisitor<'a> {
    pub(crate) fn new(
        schema: &'a schema::Schema,
        executable: &'a executable::ExecutableDocument,
        entity_query: bool,
    ) -> Option<Self> {
        Some(Self {
            schema,
            entity_query,
            fragments: executable.fragments.iter().collect(),
            found: false,
            authenticated_directive_name: Schema::directive_name(
                schema,
                AUTHENTICATED_SPEC_BASE_URL,
                AUTHENTICATED_SPEC_VERSION_RANGE,
                AUTHENTICATED_DIRECTIVE_NAME,
            )?,
        })
    }

    fn is_field_authenticated(&self, field: &schema::FieldDefinition) -> bool {
        field.directives.has(&self.authenticated_directive_name)
            || self
                .schema
                .types
                .get(field.ty.inner_named_type())
                .is_some_and(|t| self.is_type_authenticated(t))
    }

    fn is_type_authenticated(&self, t: &schema::ExtendedType) -> bool {
        t.directives().has(&self.authenticated_directive_name)
    }

    fn entities_operation(&mut self, node: &executable::Operation) -> Result<(), BoxError> {
        use crate::spec::query::traverse::Visitor;

        if node.selection_set.selections.len() != 1 {
            return Err("invalid number of selections for _entities query".into());
        }

        match node.selection_set.selections.first() {
            Some(executable::Selection::Field(field)) => {
                if field.name.as_str() != "_entities" {
                    return Err("expected _entities field".into());
                }

                for selection in &field.selection_set.selections {
                    match selection {
                        executable::Selection::InlineFragment(f) => {
                            match f.type_condition.as_ref() {
                                None => {
                                    return Err("expected type condition".into());
                                }
                                Some(condition) => self.inline_fragment(condition.as_str(), f)?,
                            };
                        }
                        _ => return Err("expected inline fragment".into()),
                    }
                }
                Ok(())
            }
            _ => Err("expected _entities field".into()),
        }
    }
}

impl<'a> traverse::Visitor for AuthenticatedCheckVisitor<'a> {
    fn operation(&mut self, root_type: &str, node: &executable::Operation) -> Result<(), BoxError> {
        if !self.entity_query {
            traverse::operation(self, root_type, node)
        } else {
            self.entities_operation(node)
        }
    }
    fn field(
        &mut self,
        _parent_type: &str,
        field_def: &ast::FieldDefinition,
        node: &executable::Field,
    ) -> Result<(), BoxError> {
        if self.is_field_authenticated(field_def) {
            self.found = true;
            return Ok(());
        }
        traverse::field(self, field_def, node)
    }

    fn fragment(&mut self, node: &executable::Fragment) -> Result<(), BoxError> {
        if self
            .schema
            .types
            .get(node.type_condition())
            .is_some_and(|type_definition| self.is_type_authenticated(type_definition))
        {
            self.found = true;
            return Ok(());
        }
        traverse::fragment(self, node)
    }

    fn fragment_spread(&mut self, node: &executable::FragmentSpread) -> Result<(), BoxError> {
        let condition = self
            .fragments
            .get(&node.fragment_name)
            .ok_or("MissingFragment")?
            .type_condition();

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
        node: &executable::InlineFragment,
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
    implementers_map: &'a HashMap<Name, Implementers>,
    pub(crate) query_requires_authentication: bool,
    pub(crate) unauthorized_paths: Vec<Path>,
    // store the error paths from fragments so we can  add them at
    // the point of application
    fragments_unauthorized_paths: HashMap<&'a ast::Name, Vec<Path>>,
    current_path: Path,
    authenticated_directive_name: String,
    dry_run: bool,
}

impl<'a> AuthenticatedVisitor<'a> {
    pub(crate) fn new(
        schema: &'a schema::Schema,
        executable: &'a ast::Document,
        implementers_map: &'a HashMap<Name, Implementers>,
        dry_run: bool,
    ) -> Option<Self> {
        Some(Self {
            schema,
            fragments: transform::collect_fragments(executable),
            implementers_map,
            dry_run,
            query_requires_authentication: false,
            unauthorized_paths: Vec::new(),
            fragments_unauthorized_paths: HashMap::new(),
            current_path: Path::default(),
            authenticated_directive_name: Schema::directive_name(
                schema,
                AUTHENTICATED_SPEC_BASE_URL,
                AUTHENTICATED_SPEC_VERSION_RANGE,
                AUTHENTICATED_DIRECTIVE_NAME,
            )?,
        })
    }

    fn is_field_authenticated(&self, field: &schema::FieldDefinition) -> bool {
        field.directives.has(&self.authenticated_directive_name)
            || self
                .schema
                .types
                .get(field.ty.inner_named_type())
                .is_some_and(|t| self.is_type_authenticated(t))
    }

    fn is_type_authenticated(&self, t: &schema::ExtendedType) -> bool {
        t.directives().has(&self.authenticated_directive_name)
    }

    fn implementors(&self, type_name: &str) -> impl Iterator<Item = &Name> {
        self.implementers_map
            .get(type_name)
            .map(|implementers| implementers.iter())
            .into_iter()
            .flatten()
    }

    fn implementors_with_different_requirements(
        &self,
        field_def: &ast::FieldDefinition,
        node: &ast::Field,
    ) -> bool {
        // we can request __typename outside of fragments even if the types have different
        // authorization requirements
        if node.name.as_str() == TYPENAME {
            return false;
        }
        // if all selections under the interface field are __typename or fragments with type conditions
        // then we don't need to check that they have the same authorization requirements
        if node.selection_set.iter().all(|sel| match sel {
            ast::Selection::Field(f) => f.name == TYPENAME,
            ast::Selection::FragmentSpread(_) | ast::Selection::InlineFragment(_) => true,
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
                .implementors(type_name)
                .filter_map(|ty| self.schema.types.get(ty))
            {
                let ty_is_authenticated = ty.directives().has(&self.authenticated_directive_name);
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

                for ty in self.implementors(parent_type) {
                    if let Ok(f) = self.schema.type_field(ty, &field.name) {
                        let field_is_authenticated =
                            f.directives.has(&self.authenticated_directive_name);
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
            .is_some_and(|ty| ty.directives.has(&self.authenticated_directive_name));

        if operation_requires_authentication {
            self.unauthorized_paths.push(self.current_path.clone());
            self.query_requires_authentication = true;
            if self.dry_run {
                transform::operation(self, root_type, node)
            } else {
                Ok(None)
            }
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
            .push(PathElement::Key(field_name.as_str().into(), None));
        if is_field_list {
            self.current_path.push(PathElement::Flatten(None));
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

            if self.dry_run {
                transform::field(self, field_def, node)
            } else {
                Ok(None)
            }
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

        let current_unauthorized_paths_index = self.unauthorized_paths.len();
        let res = if !fragment_requires_authentication || self.dry_run {
            transform::fragment_definition(self, node)
        } else {
            self.unauthorized_paths.push(self.current_path.clone());
            Ok(None)
        };

        if self.unauthorized_paths.len() > current_unauthorized_paths_index {
            if let Some((name, _)) = self.fragments.get_key_value(&node.name) {
                self.fragments_unauthorized_paths.insert(
                    name,
                    self.unauthorized_paths
                        .split_off(current_unauthorized_paths_index),
                );
            }
        }

        if let Ok(None) = res {
            self.fragments.remove(&node.name);
        }

        res
    }

    fn fragment_spread(
        &mut self,
        node: &ast::FragmentSpread,
    ) -> Result<Option<ast::FragmentSpread>, BoxError> {
        // record the fragment errors at the point of application
        if let Some(paths) = self.fragments_unauthorized_paths.get(&node.fragment_name) {
            for path in paths {
                let path = self.current_path.join(path);
                self.unauthorized_paths.push(path);
            }
        }

        let fragment = match self.fragments.get(&node.fragment_name) {
            Some(fragment) => fragment,
            None => return Ok(None),
        };

        let condition = &fragment.type_condition;

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

            if self.dry_run {
                transform::fragment_spread(self, node)
            } else {
                Ok(None)
            }
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

                    if self.dry_run {
                        transform::inline_fragment(self, parent_type, node)
                    } else {
                        Ok(None)
                    }
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
        let schema = Schema::parse_and_validate(schema, "schema.graphql").unwrap();
        let doc = ast::Document::parse(query, "query.graphql").unwrap();

        let map = schema.implementers_map();
        let mut visitor = AuthenticatedVisitor::new(&schema, &doc, &map, false).unwrap();

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
    fn fragment_fields() {
        static QUERY: &str = r#"
        query {
            topProducts {
                type
                ...F
            }
        }

        fragment F on Product {
            reviews {
                body
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

    #[test]
    fn interface_typename() {
        static SCHEMA: &str = r#"
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
        type Query {
            post(id: ID!): Post
          }
          
          interface Post {
            id: ID!
            author: String!
            title: String!
            content: String!
          }
          
          type Stats {
            views: Int
          }
          
          type PublicBlog implements Post {
            id: ID!
            author: String!
            title: String!
            content: String!
            stats: Stats @authenticated
          }
          
          type PrivateBlog implements Post @authenticated {
            id: ID!
            author: String!
            title: String!
            content: String!
            publishAt: String
          }
        "#;

        static QUERY: &str = r#"
        query Anonymous {
            post(id: "1") {
              ... on PublicBlog {
                __typename
                title
              }
            }
          }
        "#;

        let (doc, paths) = filter(SCHEMA, QUERY);

        insta::assert_display_snapshot!(TestResult {
            query: QUERY,
            result: doc,
            paths
        });

        static QUERY2: &str = r#"
        query Anonymous {
            post(id: "1") {
              __typename
              ... on PublicBlog {
                __typename
                title
              }
            }
          }
        "#;

        let (doc, paths) = filter(SCHEMA, QUERY2);

        insta::assert_display_snapshot!(TestResult {
            query: QUERY2,
            result: doc,
            paths
        });
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
                "directives": {
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
                "directives": {
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
                "directives": {
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
        headers.insert("Accept".into(), "multipart/mixed;deferSpec=20220824".into());
        context.extensions().lock().insert(ClientRequestAccepts {
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
