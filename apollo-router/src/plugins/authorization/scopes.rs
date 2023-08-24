//! Authorization plugin
//!
//! Implementation of the `@requiresScopes` directive:
//!
//! ```graphql
//! directive @requiresScopes(scopes: [[String!]!]!) on OBJECT | FIELD_DEFINITION | INTERFACE | SCALAR | ENUM
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

pub(crate) struct ScopeExtractionVisitor<'a> {
    compiler: &'a ApolloCompiler,
    file_id: FileId,
    pub(crate) extracted_scopes: HashSet<String>,
}

pub(crate) const REQUIRES_SCOPES_DIRECTIVE_NAME: &str = "requiresScopes";

impl<'a> ScopeExtractionVisitor<'a> {
    #[allow(dead_code)]
    pub(crate) fn new(compiler: &'a ApolloCompiler, file_id: FileId) -> Self {
        Self {
            compiler,
            file_id,
            extracted_scopes: HashSet::new(),
        }
    }

    fn scopes_from_field(&mut self, field: &FieldDefinition) {
        self.extracted_scopes.extend(
            scopes_argument(field.directive_by_name(REQUIRES_SCOPES_DIRECTIVE_NAME)).cloned(),
        );

        if let Some(ty) = field.ty().type_def(&self.compiler.db) {
            self.scopes_from_type(&ty)
        }
    }

    fn scopes_from_type(&mut self, ty: &TypeDefinition) {
        self.extracted_scopes
            .extend(scopes_argument(ty.directive_by_name(REQUIRES_SCOPES_DIRECTIVE_NAME)).cloned());
    }
}

fn scopes_argument(opt_directive: Option<&hir::Directive>) -> impl Iterator<Item = &String> {
    opt_directive
        .and_then(|directive| directive.argument_by_name("scopes"))
        // outer array
        .and_then(|value| match value {
            Value::List { value, .. } => Some(value),
            _ => None,
        })
        .into_iter()
        .flatten()
        // inner array
        .filter_map(|value| match value {
            Value::List { value, .. } => Some(value),
            _ => None,
        })
        .flatten()
        .filter_map(|v| match v {
            Value::String { value, .. } => Some(value),
            _ => None,
        })
}

impl<'a> traverse::Visitor for ScopeExtractionVisitor<'a> {
    fn compiler(&self) -> &ApolloCompiler {
        self.compiler
    }

    fn operation(&mut self, node: &hir::OperationDefinition) -> Result<(), BoxError> {
        if let Some(ty) = node.object_type(&self.compiler.db) {
            self.extracted_scopes.extend(
                scopes_argument(ty.directive_by_name(REQUIRES_SCOPES_DIRECTIVE_NAME)).cloned(),
            );
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
                self.scopes_from_field(field);
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
            self.scopes_from_type(ty);
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
            self.scopes_from_type(ty);
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
                self.scopes_from_type(ty);
            }
        }
        traverse::inline_fragment(self, parent_type, node)
    }
}

fn scopes_sets_argument(directive: &hir::Directive) -> impl Iterator<Item = HashSet<String>> + '_ {
    directive
        .argument_by_name("scopes")
        // outer array
        .and_then(|value| match value {
            Value::List { value, .. } => Some(value),
            _ => None,
        })
        .into_iter()
        .flatten()
        // inner array
        .filter_map(|value| match value {
            Value::List { value, .. } => Some(
                value
                    .iter()
                    .filter_map(|v| match v {
                        Value::String { value, .. } => Some(value),
                        _ => None,
                    })
                    .cloned()
                    .collect(),
            ),
            _ => None,
        })
}

pub(crate) struct ScopeFilteringVisitor<'a> {
    compiler: &'a ApolloCompiler,
    file_id: FileId,
    request_scopes: HashSet<String>,
    pub(crate) query_requires_scopes: bool,
    pub(crate) unauthorized_paths: Vec<Path>,
    current_path: Path,
}

impl<'a> ScopeFilteringVisitor<'a> {
    pub(crate) fn new(
        compiler: &'a ApolloCompiler,
        file_id: FileId,
        scopes: HashSet<String>,
    ) -> Self {
        Self {
            compiler,
            file_id,
            request_scopes: scopes,
            query_requires_scopes: false,
            unauthorized_paths: vec![],
            current_path: Path::default(),
        }
    }

    fn is_field_authorized(&mut self, field: &FieldDefinition) -> bool {
        if let Some(directive) = field.directive_by_name(REQUIRES_SCOPES_DIRECTIVE_NAME) {
            let mut field_scopes_sets = scopes_sets_argument(directive);

            // The outer array acts like a logical OR: if any of the inner arrays of scopes matches, the field
            // is authorized.
            // On an empty set, all returns true, so we must check that case separately
            let mut empty = true;
            if field_scopes_sets.all(|scopes_set| {
                empty = false;
                !self.request_scopes.is_superset(&scopes_set)
            }) && !empty
            {
                return false;
            }
        }

        if let Some(ty) = field.ty().type_def(&self.compiler.db) {
            self.is_type_authorized(&ty)
        } else {
            false
        }
    }

    fn is_type_authorized(&self, ty: &TypeDefinition) -> bool {
        match ty.directive_by_name(REQUIRES_SCOPES_DIRECTIVE_NAME) {
            None => true,
            Some(directive) => {
                let mut type_scopes_sets = scopes_sets_argument(directive);

                // The outer array acts like a logical OR: if any of the inner arrays of scopes matches, the field
                // is authorized.
                // On an empty set, any returns false, so we must check that case separately
                let mut empty = true;
                let res = type_scopes_sets.any(|scopes_set| {
                    empty = false;
                    self.request_scopes.is_superset(&scopes_set)
                });

                empty || res
            }
        }
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
            let mut scope_sets = None;

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
                let ty_scope_sets = ty
                    .directive_by_name(REQUIRES_SCOPES_DIRECTIVE_NAME)
                    .map(|directive| {
                        let mut v = scopes_sets_argument(directive)
                            .map(|h| {
                                let mut v = h.into_iter().collect::<Vec<_>>();
                                v.sort();
                                v
                            })
                            .collect::<Vec<_>>();
                        v.sort();
                        v
                    })
                    .unwrap_or_default();

                match &scope_sets {
                    None => scope_sets = Some(ty_scope_sets),
                    Some(other_scope_sets) => {
                        if ty_scope_sets != *other_scope_sets {
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
                let mut scope_sets = None;

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
                        let field_scope_sets = f
                            .directive_by_name(REQUIRES_SCOPES_DIRECTIVE_NAME)
                            .map(|directive| {
                                let mut v = scopes_sets_argument(directive)
                                    .map(|h| {
                                        let mut v = h.into_iter().collect::<Vec<_>>();
                                        v.sort();
                                        v
                                    })
                                    .collect::<Vec<_>>();
                                v.sort();
                                v
                            })
                            .unwrap_or_default();

                        match &scope_sets {
                            None => scope_sets = Some(field_scope_sets),
                            Some(other_scope_sets) => {
                                if field_scope_sets != *other_scope_sets {
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

impl<'a> transform::Visitor for ScopeFilteringVisitor<'a> {
    fn compiler(&self) -> &ApolloCompiler {
        self.compiler
    }

    fn operation(
        &mut self,
        node: &hir::OperationDefinition,
    ) -> Result<Option<apollo_encoder::OperationDefinition>, BoxError> {
        let is_authorized = if let Some(ty) = node.object_type(&self.compiler.db) {
            match ty.directive_by_name(REQUIRES_SCOPES_DIRECTIVE_NAME) {
                None => true,
                Some(directive) => {
                    let mut type_scopes_sets = scopes_sets_argument(directive);

                    // The outer array acts like a logical OR: if any of the inner arrays of scopes matches, the field
                    // is authorized.
                    // On an empty set, any returns false, so we must check that case separately
                    let mut empty = true;
                    let res = type_scopes_sets.any(|scopes_set| {
                        empty = false;
                        self.request_scopes.is_superset(&scopes_set)
                    });

                    empty || res
                }
            }
        } else {
            false
        };

        if is_authorized {
            transform::operation(self, node)
        } else {
            self.unauthorized_paths.push(self.current_path.clone());
            self.query_requires_scopes = true;
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
            self.query_requires_scopes = true;
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

        // FIXME: if a field was removed inside a fragment definition, then we should add an unauthorized path
        // starting at the fragment spread, instead of starting at the definition.
        // If we modified the transform visitor implementation to modify the fragment definitions before the
        // operations, we would be able to store the list of unauthorized paths per fragment, and at the point
        // of application, generate unauthorized paths starting at the operation root
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
            self.query_requires_scopes = true;
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
                    self.query_requires_scopes = true;
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
    use crate::plugins::authorization::scopes::ScopeExtractionVisitor;
    use crate::plugins::authorization::scopes::ScopeFilteringVisitor;
    use crate::spec::query::transform;
    use crate::spec::query::traverse;

    static BASIC_SCHEMA: &str = r#"
    scalar federation__Scope
    directive @requiresScopes(scopes: [[federation__Scope!]!]!) on OBJECT | FIELD_DEFINITION | INTERFACE | SCALAR | ENUM

    type Query {
      topProducts: Product
      customer: User
      me: User @requiresScopes(scopes: [["profile"]])
      itf: I
    }

    type Mutation @requiresScopes(scopes: [["mut"]]) {
        ping: User @requiresScopes(scopes: [["ping"]])
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

    scalar Internal @requiresScopes(scopes: [["internal", "test"]]) @specifiedBy(url: "http///example.com/test")

    type Review @requiresScopes(scopes: [["review"]]) {
        body: String
        author: User
    }

    type User implements I @requiresScopes(scopes: [["read:user"]]) {
      id: ID
      name: String @requiresScopes(scopes: [["read:username"]])
    }
    "#;

    fn extract(schema: &str, query: &str) -> BTreeSet<String> {
        let mut compiler = ApolloCompiler::new();

        let _schema_id = compiler.add_type_system(schema, "schema.graphql");
        let id = compiler.add_executable(query, "query.graphql");

        let diagnostics = compiler
            .validate()
            .into_iter()
            .filter(|err| err.data.is_error())
            .collect::<Vec<_>>();
        for diagnostic in &diagnostics {
            println!("{diagnostic}");
        }
        assert!(diagnostics.is_empty());

        let mut visitor = ScopeExtractionVisitor::new(&compiler, id);
        traverse::document(&mut visitor, id).unwrap();

        visitor.extracted_scopes.into_iter().collect()
    }

    #[test]
    fn extract_scopes() {
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

        let doc = extract(BASIC_SCHEMA, QUERY);

        insta::assert_debug_snapshot!(doc);
    }

    #[track_caller]
    fn filter(schema: &str, query: &str, scopes: HashSet<String>) -> (Document, Vec<Path>) {
        let mut compiler = ApolloCompiler::new();

        let _schema_id = compiler.add_type_system(schema, "schema.graphql");
        let file_id = compiler.add_executable(query, "query.graphql");

        let diagnostics = compiler
            .validate()
            .into_iter()
            .filter(|err| err.data.is_error())
            .collect::<Vec<_>>();
        for diagnostic in &diagnostics {
            println!("{diagnostic}");
        }
        assert!(diagnostics.is_empty());

        let mut visitor = ScopeFilteringVisitor::new(&compiler, file_id, scopes);
        (
            transform::document(&mut visitor, file_id).unwrap(),
            visitor.unauthorized_paths,
        )
    }

    struct TestResult<'a> {
        query: &'a str,
        extracted_scopes: &'a BTreeSet<String>,
        result: apollo_encoder::Document,
        scopes: Vec<String>,
        paths: Vec<Path>,
    }

    impl<'a> std::fmt::Display for TestResult<'a> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(
                f,
                "query:\n{}\nextracted_scopes: {:?}\nrequest scopes: {:?}\nfiltered:\n{}\npaths: {:?}",
                self.query,
                self.extracted_scopes,
                self.scopes,
                self.result,
                self.paths.iter().map(|p| p.to_string()).collect::<Vec<_>>()
            )
        }
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

        let extracted_scopes = extract(BASIC_SCHEMA, QUERY);
        let (doc, paths) = filter(BASIC_SCHEMA, QUERY, HashSet::new());

        insta::assert_display_snapshot!(TestResult {
            query: QUERY,
            extracted_scopes: &extracted_scopes,
            scopes: Vec::new(),
            result: doc,
            paths
        });

        let (doc, paths) = filter(
            BASIC_SCHEMA,
            QUERY,
            ["profile".to_string(), "internal".to_string()]
                .into_iter()
                .collect(),
        );

        insta::assert_display_snapshot!(TestResult {
            query: QUERY,
            extracted_scopes: &extracted_scopes,
            scopes: ["profile".to_string(), "internal".to_string()]
                .into_iter()
                .collect(),
            result: doc,
            paths
        });

        let (doc, paths) = filter(
            BASIC_SCHEMA,
            QUERY,
            [
                "profile".to_string(),
                "read:user".to_string(),
                "internal".to_string(),
                "test".to_string(),
            ]
            .into_iter()
            .collect(),
        );
        insta::assert_display_snapshot!(TestResult {
            query: QUERY,
            extracted_scopes: &extracted_scopes,
            scopes: [
                "profile".to_string(),
                "read:user".to_string(),
                "internal".to_string(),
                "test".to_string(),
            ]
            .into_iter()
            .collect(),
            result: doc,
            paths
        });

        let (doc, paths) = filter(
            BASIC_SCHEMA,
            QUERY,
            [
                "profile".to_string(),
                "read:user".to_string(),
                "read:username".to_string(),
            ]
            .into_iter()
            .collect(),
        );
        insta::assert_display_snapshot!(TestResult {
            query: QUERY,
            extracted_scopes: &extracted_scopes,
            scopes: [
                "profile".to_string(),
                "read:user".to_string(),
                "read:username".to_string(),
            ]
            .into_iter()
            .collect(),
            result: doc,
            paths
        });
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

        let extracted_scopes = extract(BASIC_SCHEMA, QUERY);

        let (doc, paths) = filter(BASIC_SCHEMA, QUERY, HashSet::new());

        insta::assert_display_snapshot!(TestResult {
            query: QUERY,
            extracted_scopes: &extracted_scopes,
            scopes: Vec::new(),
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

        let extracted_scopes = extract(BASIC_SCHEMA, QUERY);

        let (doc, paths) = filter(BASIC_SCHEMA, QUERY, HashSet::new());

        insta::assert_display_snapshot!(TestResult {
            query: QUERY,
            extracted_scopes: &extracted_scopes,
            scopes: Vec::new(),
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

        let extracted_scopes = extract(BASIC_SCHEMA, QUERY);

        let (doc, paths) = filter(BASIC_SCHEMA, QUERY, HashSet::new());

        insta::assert_display_snapshot!(TestResult {
            query: QUERY,
            extracted_scopes: &extracted_scopes,
            scopes: Vec::new(),
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

        let extracted_scopes = extract(BASIC_SCHEMA, QUERY);

        let (doc, paths) = filter(BASIC_SCHEMA, QUERY, HashSet::new());

        insta::assert_display_snapshot!(TestResult {
            query: QUERY,
            extracted_scopes: &extracted_scopes,
            scopes: Vec::new(),
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

        let extracted_scopes = extract(BASIC_SCHEMA, QUERY);

        let (doc, paths) = filter(BASIC_SCHEMA, QUERY, HashSet::new());

        insta::assert_display_snapshot!(TestResult {
            query: QUERY,
            extracted_scopes: &extracted_scopes,
            scopes: Vec::new(),
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
                    id2: id
                    name
                }
            }
        }
        "#;

        let extracted_scopes = extract(BASIC_SCHEMA, QUERY);

        let (doc, paths) = filter(BASIC_SCHEMA, QUERY, HashSet::new());

        insta::assert_display_snapshot!(TestResult {
            query: QUERY,
            extracted_scopes: &extracted_scopes,
            scopes: Vec::new(),
            result: doc,
            paths
        });

        let (doc, paths) = filter(
            BASIC_SCHEMA,
            QUERY,
            ["read:user".to_string()].into_iter().collect(),
        );

        insta::assert_display_snapshot!(TestResult {
            query: QUERY,
            extracted_scopes: &extracted_scopes,
            scopes: ["read:user".to_string()].into_iter().collect(),
            result: doc,
            paths
        });

        let (doc, paths) = filter(
            BASIC_SCHEMA,
            QUERY,
            ["read:user".to_string(), "read:username".to_string()]
                .into_iter()
                .collect(),
        );

        insta::assert_display_snapshot!(TestResult {
            query: QUERY,
            extracted_scopes: &extracted_scopes,
            scopes: ["read:user".to_string(), "read:username".to_string()]
                .into_iter()
                .collect(),
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
            id2: id
            name
        }
        "#;

        let extracted_scopes = extract(BASIC_SCHEMA, QUERY);

        let (doc, paths) = filter(BASIC_SCHEMA, QUERY, HashSet::new());

        insta::assert_display_snapshot!(TestResult {
            query: QUERY,
            extracted_scopes: &extracted_scopes,
            scopes: Vec::new(),
            result: doc,
            paths
        });

        let (doc, paths) = filter(
            BASIC_SCHEMA,
            QUERY,
            ["read:user".to_string()].into_iter().collect(),
        );

        insta::assert_display_snapshot!(TestResult {
            query: QUERY,
            extracted_scopes: &extracted_scopes,
            scopes: ["read:user".to_string()].into_iter().collect(),
            result: doc,
            paths
        });

        let (doc, paths) = filter(
            BASIC_SCHEMA,
            QUERY,
            ["read:user".to_string(), "read:username".to_string()]
                .into_iter()
                .collect(),
        );

        insta::assert_display_snapshot!(TestResult {
            query: QUERY,
            extracted_scopes: &extracted_scopes,
            scopes: ["read:user".to_string(), "read:username".to_string()]
                .into_iter()
                .collect(),
            result: doc,
            paths
        });
    }

    static INTERFACE_SCHEMA: &str = r#"
    directive @requiresScopes(scopes: [[String!]!]!) on OBJECT | FIELD_DEFINITION | INTERFACE | SCALAR | ENUM
    directive @defer on INLINE_FRAGMENT | FRAGMENT_SPREAD
    type Query {
        test: String
        itf: I!
    }
    interface I @requiresScopes(scopes: [["itf"]]) {
        id: ID
    }
    type A implements I @requiresScopes(scopes: [["a", "b"]]) {
        id: ID
        a: String
    }
    type B implements I @requiresScopes(scopes: [["c", "d"]]) {
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

        let extracted_scopes = extract(INTERFACE_SCHEMA, QUERY);
        let (doc, paths) = filter(INTERFACE_SCHEMA, QUERY, HashSet::new());

        insta::assert_display_snapshot!(TestResult {
            query: QUERY,
            extracted_scopes: &extracted_scopes,
            scopes: Vec::new(),
            result: doc,
            paths
        });

        let (doc, paths) = filter(
            INTERFACE_SCHEMA,
            QUERY,
            ["itf".to_string()].into_iter().collect(),
        );

        insta::assert_display_snapshot!(TestResult {
            query: QUERY,
            extracted_scopes: &extracted_scopes,
            scopes: ["itf".to_string()].into_iter().collect(),
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

        let extracted_scopes = extract(INTERFACE_SCHEMA, QUERY2);
        let (doc, paths) = filter(INTERFACE_SCHEMA, QUERY2, HashSet::new());

        insta::assert_display_snapshot!(TestResult {
            query: QUERY2,
            extracted_scopes: &extracted_scopes,
            scopes: Vec::new(),
            result: doc,
            paths
        });

        let (doc, paths) = filter(
            INTERFACE_SCHEMA,
            QUERY2,
            ["itf".to_string()].into_iter().collect(),
        );

        insta::assert_display_snapshot!(TestResult {
            query: QUERY2,
            extracted_scopes: &extracted_scopes,
            scopes: ["itf".to_string()].into_iter().collect(),
            result: doc,
            paths
        });

        let (doc, paths) = filter(
            INTERFACE_SCHEMA,
            QUERY2,
            ["itf".to_string(), "a".to_string(), "b".to_string()]
                .into_iter()
                .collect(),
        );

        insta::assert_display_snapshot!(TestResult {
            query: QUERY2,
            extracted_scopes: &extracted_scopes,
            scopes: ["itf".to_string(), "a".to_string(), "b".to_string()]
                .into_iter()
                .collect(),
            result: doc,
            paths
        });
    }

    static INTERFACE_FIELD_SCHEMA: &str = r#"
    directive @requiresScopes(scopes: [[String!]!]!) on OBJECT | FIELD_DEFINITION | INTERFACE | SCALAR | ENUM
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
        id: ID @requiresScopes(scopes: [["a", "b"]])
        other: String
        a: String
    }
    type B implements I {
        id: ID @requiresScopes(scopes: [["c", "d"]])
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

        let extracted_scopes: BTreeSet<String> = extract(INTERFACE_FIELD_SCHEMA, QUERY);

        let (doc, paths) = filter(INTERFACE_FIELD_SCHEMA, QUERY, HashSet::new());

        insta::assert_display_snapshot!(TestResult {
            query: QUERY,
            extracted_scopes: &extracted_scopes,
            scopes: Vec::new(),
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

        let extracted_scopes: BTreeSet<String> = extract(INTERFACE_FIELD_SCHEMA, QUERY2);

        let (doc, paths) = filter(INTERFACE_FIELD_SCHEMA, QUERY2, HashSet::new());

        insta::assert_display_snapshot!(TestResult {
            query: QUERY2,
            extracted_scopes: &extracted_scopes,
            scopes: Vec::new(),
            result: doc,
            paths
        });
    }

    #[test]
    fn union() {
        static UNION_MEMBERS_SCHEMA: &str = r#"
        directive @requiresScopes(scopes: [[String!]!]!) on OBJECT | FIELD_DEFINITION | INTERFACE | SCALAR | ENUM

        directive @defer on INLINE_FRAGMENT | FRAGMENT_SPREAD
        type Query {
            test: String
            uni: I!
        }
        union I = A | B
        type A @requiresScopes(scopes: [["a", "b"]]) {
            id: ID
        }
        type B @requiresScopes(scopes: [["c", "d"]]) {
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

        let extracted_scopes: BTreeSet<String> = extract(UNION_MEMBERS_SCHEMA, QUERY);

        let (doc, paths) = filter(
            UNION_MEMBERS_SCHEMA,
            QUERY,
            ["a".to_string(), "b".to_string()].into_iter().collect(),
        );

        insta::assert_display_snapshot!(TestResult {
            query: QUERY,
            extracted_scopes: &extracted_scopes,
            scopes: ["a".to_string(), "b".to_string()].into_iter().collect(),
            result: doc,
            paths
        });
    }
}
