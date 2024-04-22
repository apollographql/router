//! Authorization plugin
//!
//! Implementation of the `@requiresScopes` directive:
//!
//! ```graphql
//! scalar federation__Scope
//! directive @requiresScopes(scopes: [[federation__Scope!]!]!) on OBJECT | FIELD_DEFINITION | INTERFACE | SCALAR | ENUM
//! ```
use std::collections::HashMap;
use std::collections::HashSet;

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

pub(crate) struct ScopeExtractionVisitor<'a> {
    schema: &'a schema::Schema,
    fragments: HashMap<&'a ast::Name, &'a Node<executable::Fragment>>,
    pub(crate) extracted_scopes: HashSet<String>,
    requires_scopes_directive_name: String,
    entity_query: bool,
}

pub(crate) const REQUIRES_SCOPES_DIRECTIVE_NAME: &str = "requiresScopes";
pub(crate) const REQUIRES_SCOPES_SPEC_BASE_URL: &str = "https://specs.apollo.dev/requiresScopes";
pub(crate) const REQUIRES_SCOPES_SPEC_VERSION_RANGE: &str = ">=0.1.0, <=0.1.0";

impl<'a> ScopeExtractionVisitor<'a> {
    #[allow(dead_code)]
    pub(crate) fn new(
        schema: &'a schema::Schema,
        executable: &'a executable::ExecutableDocument,
        entity_query: bool,
    ) -> Option<Self> {
        Some(Self {
            schema,
            entity_query,
            fragments: executable.fragments.iter().collect(),
            extracted_scopes: HashSet::new(),
            requires_scopes_directive_name: Schema::directive_name(
                schema,
                REQUIRES_SCOPES_SPEC_BASE_URL,
                REQUIRES_SCOPES_SPEC_VERSION_RANGE,
                REQUIRES_SCOPES_DIRECTIVE_NAME,
            )?,
        })
    }

    fn scopes_from_field(&mut self, field: &schema::FieldDefinition) {
        self.extracted_scopes.extend(scopes_argument(
            field.directives.get(&self.requires_scopes_directive_name),
        ));

        if let Some(ty) = self.schema.types.get(field.ty.inner_named_type()) {
            self.scopes_from_type(ty)
        }
    }

    fn scopes_from_type(&mut self, ty: &schema::ExtendedType) {
        self.extracted_scopes.extend(scopes_argument(
            ty.directives().get(&self.requires_scopes_directive_name),
        ));
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

fn scopes_argument(
    opt_directive: Option<&impl AsRef<ast::Directive>>,
) -> impl Iterator<Item = String> + '_ {
    opt_directive
        .and_then(|directive| directive.as_ref().argument_by_name("scopes"))
        // outer array
        .and_then(|value| value.as_list())
        .into_iter()
        .flatten()
        // inner array
        .filter_map(|value| value.as_list())
        .flatten()
        .filter_map(|value| value.as_str().map(str::to_owned))
}

impl<'a> traverse::Visitor for ScopeExtractionVisitor<'a> {
    fn operation(&mut self, root_type: &str, node: &executable::Operation) -> Result<(), BoxError> {
        if let Some(ty) = self.schema.types.get(root_type) {
            self.extracted_scopes.extend(scopes_argument(
                ty.directives().get(&self.requires_scopes_directive_name),
            ));
        }

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
        self.scopes_from_field(field_def);

        traverse::field(self, field_def, node)
    }

    fn fragment(&mut self, node: &executable::Fragment) -> Result<(), BoxError> {
        if let Some(ty) = self.schema.types.get(node.type_condition()) {
            self.scopes_from_type(ty);
        }
        traverse::fragment(self, node)
    }

    fn fragment_spread(&mut self, node: &executable::FragmentSpread) -> Result<(), BoxError> {
        let type_condition = self
            .fragments
            .get(&node.fragment_name)
            .ok_or("MissingFragment")?
            .type_condition();

        if let Some(ty) = self.schema.types.get(type_condition) {
            self.scopes_from_type(ty);
        }
        traverse::fragment_spread(self, node)
    }

    fn inline_fragment(
        &mut self,
        parent_type: &str,
        node: &executable::InlineFragment,
    ) -> Result<(), BoxError> {
        if let Some(type_condition) = &node.type_condition {
            if let Some(ty) = self.schema.types.get(type_condition) {
                self.scopes_from_type(ty);
            }
        }
        traverse::inline_fragment(self, parent_type, node)
    }

    fn schema(&self) -> &apollo_compiler::Schema {
        self.schema
    }
}

fn scopes_sets_argument(directive: &ast::Directive) -> impl Iterator<Item = HashSet<String>> + '_ {
    directive
        .argument_by_name("scopes")
        // outer array
        .and_then(|value| value.as_list())
        .into_iter()
        .flatten()
        // inner array
        .filter_map(|value| {
            value.as_list().map(|list| {
                list.iter()
                    .filter_map(|value| value.as_str().map(str::to_owned))
                    .collect()
            })
        })
}

pub(crate) struct ScopeFilteringVisitor<'a> {
    schema: &'a schema::Schema,
    fragments: HashMap<&'a ast::Name, &'a ast::FragmentDefinition>,
    implementers_map: &'a HashMap<Name, Implementers>,
    request_scopes: HashSet<String>,
    pub(crate) query_requires_scopes: bool,
    pub(crate) unauthorized_paths: Vec<Path>,
    // store the error paths from fragments so we can  add them at
    // the point of application
    fragments_unauthorized_paths: HashMap<&'a ast::Name, Vec<Path>>,
    current_path: Path,
    requires_scopes_directive_name: String,
    dry_run: bool,
}

impl<'a> ScopeFilteringVisitor<'a> {
    pub(crate) fn new(
        schema: &'a schema::Schema,
        executable: &'a ast::Document,
        implementers_map: &'a HashMap<Name, Implementers>,
        scopes: HashSet<String>,
        dry_run: bool,
    ) -> Option<Self> {
        Some(Self {
            schema,
            fragments: transform::collect_fragments(executable),
            implementers_map,
            request_scopes: scopes,
            dry_run,
            query_requires_scopes: false,
            unauthorized_paths: vec![],
            fragments_unauthorized_paths: HashMap::new(),
            current_path: Path::default(),
            requires_scopes_directive_name: Schema::directive_name(
                schema,
                REQUIRES_SCOPES_SPEC_BASE_URL,
                REQUIRES_SCOPES_SPEC_VERSION_RANGE,
                REQUIRES_SCOPES_DIRECTIVE_NAME,
            )?,
        })
    }

    fn is_field_authorized(&mut self, field: &schema::FieldDefinition) -> bool {
        if let Some(directive) = field.directives.get(&self.requires_scopes_directive_name) {
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

        if let Some(ty) = self.schema.types.get(field.ty.inner_named_type()) {
            self.is_type_authorized(ty)
        } else {
            false
        }
    }

    fn is_type_authorized(&self, ty: &schema::ExtendedType) -> bool {
        match ty.directives().get(&self.requires_scopes_directive_name) {
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

        let field_type = field_def.ty.inner_named_type();
        if let Some(type_definition) = self.schema.types.get(field_type) {
            if self.implementors_with_different_type_requirements(field_def, type_definition) {
                return true;
            }
        }
        false
    }

    fn implementors_with_different_type_requirements(
        &self,
        field_def: &ast::FieldDefinition,
        t: &schema::ExtendedType,
    ) -> bool {
        if t.is_interface() {
            let mut scope_sets = None;
            let type_name = field_def.ty.inner_named_type();

            for ty in self
                .implementors(type_name)
                .filter_map(|ty| self.schema.types.get(ty))
            {
                // aggregate the list of scope sets
                // we transform to a common representation of sorted vectors because the element order
                // of hashsets is not stable
                let ty_scope_sets = ty
                    .directives()
                    .get(&self.requires_scopes_directive_name)
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
        field: &ast::Field,
    ) -> bool {
        if let Some(t) = self.schema.types.get(parent_type) {
            if t.is_interface() {
                let mut scope_sets = None;

                for ty in self.implementors(parent_type) {
                    if let Ok(f) = self.schema.type_field(ty, &field.name) {
                        // aggregate the list of scope sets
                        // we transform to a common representation of sorted vectors because the element order
                        // of hashsets is not stable
                        let field_scope_sets = f
                            .directives
                            .get(&self.requires_scopes_directive_name)
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
    fn operation(
        &mut self,
        root_type: &str,
        node: &ast::OperationDefinition,
    ) -> Result<Option<ast::OperationDefinition>, BoxError> {
        let is_authorized = if let Some(ty) = self.schema.types.get(root_type) {
            match ty.directives().get(&self.requires_scopes_directive_name) {
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
            transform::operation(self, root_type, node)
        } else {
            self.unauthorized_paths.push(self.current_path.clone());
            self.query_requires_scopes = true;

            if self.dry_run {
                transform::operation(self, root_type, node)
            } else {
                Ok(None)
            }
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
            .push(PathElement::Key(field_name.as_str().into(), None));
        if is_field_list {
            self.current_path.push(PathElement::Flatten(None));
        }

        let res = if is_authorized
            && !implementors_with_different_requirements
            && !implementors_with_different_field_requirements
        {
            transform::field(self, field_def, node)
        } else {
            self.unauthorized_paths.push(self.current_path.clone());
            self.query_requires_scopes = true;

            if self.dry_run {
                transform::field(self, field_def, node)
            } else {
                Ok(None)
            }
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

        let current_unauthorized_paths_index = self.unauthorized_paths.len();

        let res = if fragment_is_authorized || self.dry_run {
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

        let fragment_is_authorized = self
            .schema
            .types
            .get(condition)
            .is_some_and(|ty| self.is_type_authorized(ty));

        let res = if !fragment_is_authorized {
            self.query_requires_scopes = true;
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

                let fragment_is_authorized = self
                    .schema
                    .types
                    .get(name)
                    .is_some_and(|ty| self.is_type_authorized(ty));

                let res = if !fragment_is_authorized {
                    self.query_requires_scopes = true;
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
    use std::collections::BTreeSet;
    use std::collections::HashSet;

    use apollo_compiler::ast::Document;
    use apollo_compiler::Schema;

    use crate::json_ext::Path;
    use crate::plugins::authorization::scopes::ScopeExtractionVisitor;
    use crate::plugins::authorization::scopes::ScopeFilteringVisitor;
    use crate::spec::query::transform;
    use crate::spec::query::traverse;

    static BASIC_SCHEMA: &str = r#"
    schema
      @link(url: "https://specs.apollo.dev/link/v1.0")
      @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION)
      @link(url: "https://specs.apollo.dev/requiresScopes/v0.1", for: SECURITY)
    {
        query: Query
        mutation: Mutation
    }
    directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
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
        let schema = Schema::parse_and_validate(schema, "schema.graphql").unwrap();
        let doc = Document::parse(query, "query.graphql").unwrap();
        let exec = doc.to_executable_validate(&schema).unwrap();
        let mut visitor = ScopeExtractionVisitor::new(&schema, &exec, false).unwrap();
        traverse::document(&mut visitor, &exec, None).unwrap();

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
        let schema = Schema::parse_and_validate(schema, "schema.graphql").unwrap();
        let doc = Document::parse(query, "query.graphql").unwrap();
        doc.to_executable_validate(&schema).unwrap();

        let map = schema.implementers_map();
        let mut visitor = ScopeFilteringVisitor::new(&schema, &doc, &map, scopes, false).unwrap();
        (
            transform::document(&mut visitor, &doc).unwrap(),
            visitor.unauthorized_paths,
        )
    }

    struct TestResult<'a> {
        query: &'a str,
        extracted_scopes: &'a BTreeSet<String>,
        result: Document,
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

    static INTERFACE_SCHEMA: &str = r#"
    schema
    @link(url: "https://specs.apollo.dev/link/v1.0")
    @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION)
    @link(url: "https://specs.apollo.dev/requiresScopes/v0.1", for: SECURITY)
    {
      query: Query
    }
    directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
    directive @requiresScopes(scopes: [[String!]!]!) on OBJECT | FIELD_DEFINITION | INTERFACE | SCALAR | ENUM
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
    schema
    @link(url: "https://specs.apollo.dev/link/v1.0")
    @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION)
    @link(url: "https://specs.apollo.dev/requiresScopes/v0.1", for: SECURITY)
    {
      query: Query
    }
    directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
    directive @requiresScopes(scopes: [[String!]!]!) on OBJECT | FIELD_DEFINITION | INTERFACE | SCALAR | ENUM
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
        schema
        @link(url: "https://specs.apollo.dev/link/v1.0")
        @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION)
        @link(url: "https://specs.apollo.dev/requiresScopes/v0.1", for: SECURITY)
        {
          query: Query
        }
        directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
        directive @requiresScopes(scopes: [[String!]!]!) on OBJECT | FIELD_DEFINITION | INTERFACE | SCALAR | ENUM
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

    static RENAMED_SCHEMA: &str = r#"
    schema
      @link(url: "https://specs.apollo.dev/link/v1.0")
      @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION)
      @link(url: "https://specs.apollo.dev/requiresScopes/v0.1", as: "scopes" for: SECURITY)
    {
        query: Query
        mutation: Mutation
    }
    directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
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
    scalar federation__Scope
    directive @scopes(scopes: [[federation__Scope!]!]!) on OBJECT | FIELD_DEFINITION | INTERFACE | SCALAR | ENUM

    type Query {
      topProducts: Product
      customer: User
      me: User @scopes(scopes: [["profile"]])
      itf: I
    }

    type Mutation @scopes(scopes: [["mut"]]) {
        ping: User @scopes(scopes: [["ping"]])
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

    scalar Internal @scopes(scopes: [["internal", "test"]]) @specifiedBy(url: "http///example.com/test")

    type Review @scopes(scopes: [["review"]]) {
        body: String
        author: User
    }

    type User implements I @scopes(scopes: [["read:user"]]) {
      id: ID
      name: String @scopes(scopes: [["read:username"]])
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

        let extracted_scopes = extract(RENAMED_SCHEMA, QUERY);

        let (doc, paths) = filter(RENAMED_SCHEMA, QUERY, HashSet::new());

        insta::assert_display_snapshot!(TestResult {
            query: QUERY,
            extracted_scopes: &extracted_scopes,
            scopes: Vec::new(),
            result: doc,
            paths
        });
    }

    #[test]
    fn interface_typename() {
        static SCHEMA: &str = r#"
        schema
    @link(url: "https://specs.apollo.dev/link/v1.0")
    @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION)
    @link(url: "https://specs.apollo.dev/requiresScopes/v0.1", for: SECURITY)
    {
      query: Query
    }
    directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
    directive @requiresScopes(scopes: [[String!]!]!) on OBJECT | FIELD_DEFINITION | INTERFACE | SCALAR | ENUM
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
            stats: Stats @requiresScopes(scopes: [["a"]])
          }
          
          type PrivateBlog implements Post @requiresScopes(scopes: [["b"]]) {
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

        let extracted_scopes: BTreeSet<String> = extract(SCHEMA, QUERY);

        let (doc, paths) = filter(SCHEMA, QUERY, HashSet::new());

        insta::assert_display_snapshot!(TestResult {
            query: QUERY,
            extracted_scopes: &extracted_scopes,
            scopes: Vec::new(),
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

        let extracted_scopes: BTreeSet<String> = extract(SCHEMA, QUERY2);

        let (doc, paths) = filter(SCHEMA, QUERY2, HashSet::new());

        insta::assert_display_snapshot!(TestResult {
            query: QUERY2,
            extracted_scopes: &extracted_scopes,
            scopes: Vec::new(),
            result: doc,
            paths
        });
    }
}
