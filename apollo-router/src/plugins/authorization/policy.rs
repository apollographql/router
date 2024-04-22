//! Authorization plugin
//!
//! Implementation of the `@policy` directive:
//!
//! ```graphql
//! scalar federation__Policy
//! directive @policy(policies: [[federation__Policy!]!]!) on OBJECT | FIELD_DEFINITION | INTERFACE | SCALAR | ENUM
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

pub(crate) struct PolicyExtractionVisitor<'a> {
    schema: &'a schema::Schema,
    fragments: HashMap<&'a ast::Name, &'a Node<executable::Fragment>>,
    pub(crate) extracted_policies: HashSet<String>,
    policy_directive_name: String,
    entity_query: bool,
}

pub(crate) const POLICY_DIRECTIVE_NAME: &str = "policy";
pub(crate) const POLICY_SPEC_BASE_URL: &str = "https://specs.apollo.dev/policy";
pub(crate) const POLICY_SPEC_VERSION_RANGE: &str = ">=0.1.0, <=0.1.0";

impl<'a> PolicyExtractionVisitor<'a> {
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
            extracted_policies: HashSet::new(),
            policy_directive_name: Schema::directive_name(
                schema,
                POLICY_SPEC_BASE_URL,
                POLICY_SPEC_VERSION_RANGE,
                POLICY_DIRECTIVE_NAME,
            )?,
        })
    }

    fn get_policies_from_field(&mut self, field: &schema::FieldDefinition) {
        self.extracted_policies.extend(policy_argument(
            field.directives.get(&self.policy_directive_name),
        ));

        if let Some(ty) = self.schema.types.get(field.ty.inner_named_type()) {
            self.get_policies_from_type(ty)
        }
    }

    fn get_policies_from_type(&mut self, ty: &schema::ExtendedType) {
        self.extracted_policies.extend(policy_argument(
            ty.directives().get(&self.policy_directive_name),
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

fn policy_argument(
    opt_directive: Option<&impl AsRef<ast::Directive>>,
) -> impl Iterator<Item = String> + '_ {
    opt_directive
        .and_then(|directive| directive.as_ref().argument_by_name("policies"))
        // outer array
        .and_then(|value| value.as_list())
        .into_iter()
        .flatten()
        // inner array
        .filter_map(|value| value.as_list())
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_owned))
}

impl<'a> traverse::Visitor for PolicyExtractionVisitor<'a> {
    fn operation(&mut self, root_type: &str, node: &executable::Operation) -> Result<(), BoxError> {
        if let Some(ty) = self.schema.types.get(root_type) {
            self.extracted_policies.extend(policy_argument(
                ty.directives().get(&self.policy_directive_name),
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
        self.get_policies_from_field(field_def);

        traverse::field(self, field_def, node)
    }

    fn fragment(&mut self, node: &executable::Fragment) -> Result<(), BoxError> {
        if let Some(ty) = self.schema.types.get(node.type_condition()) {
            self.get_policies_from_type(ty);
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
            self.get_policies_from_type(ty);
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
    implementers_map: &'a HashMap<Name, Implementers>,
    dry_run: bool,
    request_policies: HashSet<String>,
    pub(crate) query_requires_policies: bool,
    pub(crate) unauthorized_paths: Vec<Path>,
    // store the error paths from fragments so we can  add them at
    // the point of application
    fragments_unauthorized_paths: HashMap<&'a ast::Name, Vec<Path>>,
    current_path: Path,
    policy_directive_name: String,
}

fn policies_sets_argument(
    directive: &ast::Directive,
) -> impl Iterator<Item = HashSet<String>> + '_ {
    directive
        .argument_by_name("policies")
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

impl<'a> PolicyFilteringVisitor<'a> {
    pub(crate) fn new(
        schema: &'a schema::Schema,
        executable: &'a ast::Document,
        implementers_map: &'a HashMap<Name, Implementers>,
        successful_policies: HashSet<String>,
        dry_run: bool,
    ) -> Option<Self> {
        Some(Self {
            schema,
            fragments: transform::collect_fragments(executable),
            implementers_map,
            dry_run,
            request_policies: successful_policies,
            query_requires_policies: false,
            unauthorized_paths: vec![],
            fragments_unauthorized_paths: HashMap::new(),
            current_path: Path::default(),
            policy_directive_name: Schema::directive_name(
                schema,
                POLICY_SPEC_BASE_URL,
                POLICY_SPEC_VERSION_RANGE,
                POLICY_DIRECTIVE_NAME,
            )?,
        })
    }

    fn is_field_authorized(&mut self, field: &schema::FieldDefinition) -> bool {
        if let Some(directive) = field.directives.get(&self.policy_directive_name) {
            let mut field_policies_sets = policies_sets_argument(directive);

            // The outer array acts like a logical OR: if any of the inner arrays of policies matches, the field
            // is authorized.
            // On an empty set, all returns true, so we must check that case separately
            let mut empty = true;
            if field_policies_sets.all(|policies_set| {
                empty = false;
                !self.request_policies.is_superset(&policies_set)
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
        match ty.directives().get(&self.policy_directive_name) {
            None => true,
            Some(directive) => {
                let mut type_policies_sets = policies_sets_argument(directive);

                // The outer array acts like a logical OR: if any of the inner arrays of policies matches, the field
                // is authorized.
                // On an empty set, any returns false, so we must check that case separately
                let mut empty = true;
                let res = type_policies_sets.any(|policies_set| {
                    empty = false;
                    self.request_policies.is_superset(&policies_set)
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
            let mut policies_sets: Option<Vec<Vec<String>>> = None;

            for ty in self
                .implementors(type_name)
                .filter_map(|ty| self.schema.types.get(ty))
            {
                // aggregate the list of policies sets
                // we transform to a common representation of sorted vectors because the element order
                // of hashsets is not stable
                let ty_policies_sets = ty
                    .directives()
                    .get(&self.policy_directive_name)
                    .map(|directive| {
                        let mut v = policies_sets_argument(directive)
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

                match &policies_sets {
                    None => policies_sets = Some(ty_policies_sets),
                    Some(other_policies) => {
                        if ty_policies_sets != *other_policies {
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
                let mut policies_sets: Option<Vec<Vec<String>>> = None;

                for ty in self.implementors(parent_type) {
                    if let Ok(f) = self.schema.type_field(ty, &field.name) {
                        // aggregate the list of policies sets
                        // we transform to a common representation of sorted vectors because the element order
                        // of hashsets is not stable
                        let field_policies = f
                            .directives
                            .get(&self.policy_directive_name)
                            .map(|directive| {
                                let mut v = policies_sets_argument(directive)
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

                        match &policies_sets {
                            None => policies_sets = Some(field_policies),
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
            match ty.directives.get(&self.policy_directive_name) {
                None => true,
                Some(directive) => {
                    let mut type_policies_sets = policies_sets_argument(directive);

                    // The outer array acts like a logical OR: if any of the inner arrays of policies matches, the field
                    // is authorized.
                    // On an empty set, any returns false, so we must check that case separately
                    let mut empty = true;
                    let res = type_policies_sets.any(|policies_set| {
                        empty = false;
                        self.request_policies.is_superset(&policies_set)
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
            self.query_requires_policies = true;

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
            self.query_requires_policies = true;

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
            self.query_requires_policies = true;
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
                    self.query_requires_policies = true;
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

    use apollo_compiler::ast;
    use apollo_compiler::ast::Document;
    use apollo_compiler::ExecutableDocument;
    use apollo_compiler::Schema;

    use crate::json_ext::Path;
    use crate::plugins::authorization::policy::PolicyExtractionVisitor;
    use crate::plugins::authorization::policy::PolicyFilteringVisitor;
    use crate::spec::query::transform;
    use crate::spec::query::traverse;

    static BASIC_SCHEMA: &str = r#"
    schema
      @link(url: "https://specs.apollo.dev/link/v1.0")
      @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION)
      @link(url: "https://specs.apollo.dev/policy/v0.1", for: SECURITY)
    {
      query: Query
      mutation: Mutation
    }
    directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
    directive @policy(policies: [[String!]!]!) on OBJECT | FIELD_DEFINITION | INTERFACE | SCALAR | ENUM
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
      customer: User @policy(policies: [["read user", "internal"], ["admin"]])
      me: User @policy(policies: [["profile"]])
      itf: I
    }

    type Mutation @policy(policies: [["mut"]]) {
        ping: User @policy(policies: [["ping"]])
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

    scalar Internal @policy(policies: [["internal"]]) @specifiedBy(url: "http///example.com/test")

    type Review @policy(policies: [["review"]]) {
        body: String
        author: User
    }

    type User implements I @policy(policies: [["read user"]]) {
      id: ID
      name: String @policy(policies: [["read username"]])
    }
    "#;

    fn extract(schema: &str, query: &str) -> BTreeSet<String> {
        let schema = Schema::parse_and_validate(schema, "schema.graphql").unwrap();
        let doc = ExecutableDocument::parse(&schema, query, "query.graphql").unwrap();
        let mut visitor = PolicyExtractionVisitor::new(&schema, &doc, false).unwrap();
        traverse::document(&mut visitor, &doc, None).unwrap();

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

        let doc = extract(BASIC_SCHEMA, QUERY);
        insta::assert_debug_snapshot!(doc);
    }

    #[track_caller]
    fn filter(schema: &str, query: &str, policies: HashSet<String>) -> (ast::Document, Vec<Path>) {
        let schema = Schema::parse_and_validate(schema, "schema.graphql").unwrap();
        let doc = ast::Document::parse(query, "query.graphql").unwrap();
        doc.to_executable_validate(&schema).unwrap();
        let map = schema.implementers_map();
        let mut visitor =
            PolicyFilteringVisitor::new(&schema, &doc, &map, policies, false).unwrap();
        (
            transform::document(&mut visitor, &doc).unwrap(),
            visitor.unauthorized_paths,
        )
    }

    struct TestResult<'a> {
        query: &'a str,
        extracted_policies: &'a BTreeSet<String>,
        result: Document,
        successful_policies: Vec<String>,
        paths: Vec<Path>,
    }

    impl<'a> std::fmt::Display for TestResult<'a> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(
                f,
                "query:\n{}\nextracted_policies: {:?}\nsuccessful policies: {:?}\nfiltered:\n{}\npaths: {:?}",
                self.query,
                self.extracted_policies,
                self.successful_policies,
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

        let extracted_policies = extract(BASIC_SCHEMA, QUERY);
        let (doc, paths) = filter(BASIC_SCHEMA, QUERY, HashSet::new());
        insta::assert_display_snapshot!(TestResult {
            query: QUERY,
            extracted_policies: &extracted_policies,
            successful_policies: Vec::new(),
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
            extracted_policies: &extracted_policies,
            successful_policies: ["profile".to_string(), "internal".to_string()]
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
                "read user".to_string(),
                "internal".to_string(),
            ]
            .into_iter()
            .collect(),
        );
        insta::assert_display_snapshot!(TestResult {
            query: QUERY,
            extracted_policies: &extracted_policies,
            successful_policies: [
                "profile".to_string(),
                "read user".to_string(),
                "internal".to_string()
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
                "read user".to_string(),
                "read username".to_string(),
            ]
            .into_iter()
            .collect(),
        );
        insta::assert_display_snapshot!(TestResult {
            query: QUERY,
            extracted_policies: &extracted_policies,
            successful_policies: [
                "profile".to_string(),
                "read user".to_string(),
                "read username".to_string(),
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

        let extracted_policies = extract(BASIC_SCHEMA, QUERY);
        let (doc, paths) = filter(BASIC_SCHEMA, QUERY, HashSet::new());

        insta::assert_display_snapshot!(TestResult {
            query: QUERY,
            extracted_policies: &extracted_policies,
            successful_policies: Vec::new(),
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

        let extracted_policies = extract(BASIC_SCHEMA, QUERY);
        let (doc, paths) = filter(BASIC_SCHEMA, QUERY, HashSet::new());

        insta::assert_display_snapshot!(TestResult {
            query: QUERY,
            extracted_policies: &extracted_policies,
            successful_policies: Vec::new(),
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

        let extracted_policies = extract(BASIC_SCHEMA, QUERY);
        let (doc, paths) = filter(BASIC_SCHEMA, QUERY, HashSet::new());
        insta::assert_display_snapshot!(TestResult {
            query: QUERY,
            extracted_policies: &extracted_policies,
            successful_policies: Vec::new(),
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

        let extracted_policies = extract(BASIC_SCHEMA, QUERY);
        let (doc, paths) = filter(BASIC_SCHEMA, QUERY, HashSet::new());
        insta::assert_display_snapshot!(TestResult {
            query: QUERY,
            extracted_policies: &extracted_policies,
            successful_policies: Vec::new(),
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

        let extracted_policies = extract(BASIC_SCHEMA, QUERY);
        let (doc, paths) = filter(BASIC_SCHEMA, QUERY, HashSet::new());
        insta::assert_display_snapshot!(TestResult {
            query: QUERY,
            extracted_policies: &extracted_policies,
            successful_policies: Vec::new(),
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

        let extracted_policies = extract(BASIC_SCHEMA, QUERY);
        let (doc, paths) = filter(BASIC_SCHEMA, QUERY, HashSet::new());
        insta::assert_display_snapshot!(TestResult {
            query: QUERY,
            extracted_policies: &extracted_policies,
            successful_policies: Vec::new(),
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

        let extracted_policies = extract(BASIC_SCHEMA, QUERY);
        let (doc, paths) = filter(BASIC_SCHEMA, QUERY, HashSet::new());
        insta::assert_display_snapshot!(TestResult {
            query: QUERY,
            extracted_policies: &extracted_policies,
            successful_policies: Vec::new(),
            result: doc,
            paths
        });

        let (doc, paths) = filter(
            BASIC_SCHEMA,
            QUERY,
            ["read user".to_string(), "read username".to_string()]
                .into_iter()
                .collect(),
        );
        insta::assert_display_snapshot!(TestResult {
            query: QUERY,
            extracted_policies: &extracted_policies,
            successful_policies: ["read user".to_string(), "read username".to_string()]
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

        let extracted_policies = extract(BASIC_SCHEMA, QUERY);
        let (doc, paths) = filter(BASIC_SCHEMA, QUERY, HashSet::new());

        insta::assert_display_snapshot!(TestResult {
            query: QUERY,
            extracted_policies: &extracted_policies,
            successful_policies: Vec::new(),
            result: doc,
            paths
        });
    }

    #[test]
    fn or_and() {
        static QUERY: &str = r#"
        {
            customer {
                id
            }
        }
        "#;

        let extracted_policies = extract(BASIC_SCHEMA, QUERY);
        let (doc, paths) = filter(BASIC_SCHEMA, QUERY, HashSet::new());
        insta::assert_display_snapshot!(TestResult {
            query: QUERY,
            extracted_policies: &extracted_policies,
            successful_policies: Vec::new(),
            result: doc,
            paths
        });

        let (doc, paths) = filter(
            BASIC_SCHEMA,
            QUERY,
            ["read user".to_string(), "internal".to_string()]
                .into_iter()
                .collect(),
        );
        insta::assert_display_snapshot!(TestResult {
            query: QUERY,
            extracted_policies: &extracted_policies,
            successful_policies: ["read user".to_string(), "internal".to_string()]
                .into_iter()
                .collect(),
            result: doc,
            paths
        });

        let (doc, paths) = filter(
            BASIC_SCHEMA,
            QUERY,
            ["read user".to_string()].into_iter().collect(),
        );
        insta::assert_display_snapshot!(TestResult {
            query: QUERY,
            extracted_policies: &extracted_policies,
            successful_policies: ["read user".to_string(),].into_iter().collect(),
            result: doc,
            paths
        });

        let (doc, paths) = filter(
            BASIC_SCHEMA,
            QUERY,
            ["admin".to_string(), "read user".to_string()]
                .into_iter()
                .collect(),
        );
        insta::assert_display_snapshot!(TestResult {
            query: QUERY,
            extracted_policies: &extracted_policies,
            successful_policies: ["admin".to_string(), "read user".to_string()]
                .into_iter()
                .collect(),
            result: doc,
            paths
        });
    }

    static INTERFACE_SCHEMA: &str = r#"
    schema
      @link(url: "https://specs.apollo.dev/link/v1.0")
      @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION)
      @link(url: "https://specs.apollo.dev/policy/v0.1", for: SECURITY)
    {
      query: Query
    }
    directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
    directive @policy(policies: [[String!]!]!) on OBJECT | FIELD_DEFINITION | INTERFACE | SCALAR | ENUM
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
    interface I @policy(policies: [["itf"]]) {
        id: ID
    }
    type A implements I @policy(policies: [["a"]]) {
        id: ID
        a: String
    }
    type B implements I @policy(policies: [["b"]]) {
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

        let extracted_policies = extract(INTERFACE_SCHEMA, QUERY);
        let (doc, paths) = filter(INTERFACE_SCHEMA, QUERY, HashSet::new());
        insta::assert_display_snapshot!(TestResult {
            query: QUERY,
            extracted_policies: &extracted_policies,
            successful_policies: Vec::new(),
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
            extracted_policies: &extracted_policies,
            successful_policies: ["itf".to_string()].into_iter().collect(),
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

        let extracted_policies = extract(INTERFACE_SCHEMA, QUERY2);
        let (doc, paths) = filter(INTERFACE_SCHEMA, QUERY2, HashSet::new());
        insta::assert_display_snapshot!(TestResult {
            query: QUERY2,
            extracted_policies: &extracted_policies,
            successful_policies: Vec::new(),
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
            extracted_policies: &extracted_policies,
            successful_policies: ["itf".to_string()].into_iter().collect(),
            result: doc,
            paths
        });

        let (doc, paths) = filter(
            INTERFACE_SCHEMA,
            QUERY2,
            ["itf".to_string(), "a".to_string()].into_iter().collect(),
        );
        insta::assert_display_snapshot!(TestResult {
            query: QUERY2,
            extracted_policies: &extracted_policies,
            successful_policies: ["itf".to_string(), "a".to_string()].into_iter().collect(),
            result: doc,
            paths
        });
    }

    static INTERFACE_FIELD_SCHEMA: &str = r#"
    schema
      @link(url: "https://specs.apollo.dev/link/v1.0")
      @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION)
      @link(url: "https://specs.apollo.dev/policy/v0.1", for: SECURITY)
    {
      query: Query
    }
    directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
    directive @policy(policies: [[String!]!]!) on OBJECT | FIELD_DEFINITION | INTERFACE | SCALAR | ENUM
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
        id: ID @policy(policies: [["a"]])
        other: String
        a: String
    }
    type B implements I {
        id: ID @policy(policies: [["b"]])
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

        let extracted_policies = extract(INTERFACE_FIELD_SCHEMA, QUERY);
        let (doc, paths) = filter(INTERFACE_FIELD_SCHEMA, QUERY, HashSet::new());
        insta::assert_display_snapshot!(TestResult {
            query: QUERY,
            extracted_policies: &extracted_policies,
            successful_policies: Vec::new(),
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

        let extracted_policies = extract(INTERFACE_FIELD_SCHEMA, QUERY2);
        let (doc, paths) = filter(INTERFACE_FIELD_SCHEMA, QUERY2, HashSet::new());
        insta::assert_display_snapshot!(TestResult {
            query: QUERY2,
            extracted_policies: &extracted_policies,
            successful_policies: Vec::new(),
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
          @link(url: "https://specs.apollo.dev/policy/v0.1", for: SECURITY)
        {
          query: Query
        }
        directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
        directive @policy(policies: [[String!]!]!) on OBJECT | FIELD_DEFINITION | INTERFACE | SCALAR | ENUM
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
        type A @policy(policies: [["a"]]) {
            id: ID
        }
        type B @policy(policies: [["b"]]) {
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

        let extracted_policies = extract(UNION_MEMBERS_SCHEMA, QUERY);
        let (doc, paths) = filter(
            UNION_MEMBERS_SCHEMA,
            QUERY,
            ["a".to_string()].into_iter().collect(),
        );
        insta::assert_display_snapshot!(TestResult {
            query: QUERY,
            extracted_policies: &extracted_policies,
            successful_policies: ["a".to_string()].into_iter().collect(),
            result: doc,
            paths
        });
    }

    static RENAMED_SCHEMA: &str = r#"
      schema
        @link(url: "https://specs.apollo.dev/link/v1.0")
        @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION)
        @link(url: "https://specs.apollo.dev/policy/v0.1", as: "policies" for: SECURITY)
      {
          query: Query
          mutation: Mutation
      }
      directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
      directive @policies(policies: [[String!]!]!) on OBJECT | FIELD_DEFINITION | INTERFACE | SCALAR | ENUM
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
        me: User @policies(policies: [["profile"]])
        itf: I
      }
      type Mutation @policies(policies: [["mut"]]) {
          ping: User @policies(policies: [["ping"]])
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
      scalar Internal @policies(policies: [["internal"], ["test"]]) @specifiedBy(url: "http///example.com/test")
      type Review @policies(policies: ["review"]) {
          body: String
          author: User
      }
      type User implements I @policies(policies: [["read:user"]]) {
        id: ID
        name: String @policies(policies: [["read:username"]])
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

        let extracted_policies = extract(RENAMED_SCHEMA, QUERY);
        let (doc, paths) = filter(RENAMED_SCHEMA, QUERY, HashSet::new());

        insta::assert_display_snapshot!(TestResult {
            query: QUERY,
            extracted_policies: &extracted_policies,
            successful_policies: Vec::new(),
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
        @link(url: "https://specs.apollo.dev/policy/v0.1", for: SECURITY)
      {
        query: Query
      }
      directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
      directive @policy(policies: [String]) on OBJECT | FIELD_DEFINITION | INTERFACE | SCALAR | ENUM
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
            stats: Stats @policy(policies: ["a"])
          }
          
          type PrivateBlog implements Post @policy(policies: ["b"]) {
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

        let (doc, paths) = filter(SCHEMA, QUERY, HashSet::new());

        insta::assert_display_snapshot!(doc);
        insta::assert_debug_snapshot!(paths);

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

        let (doc, paths) = filter(SCHEMA, QUERY2, HashSet::new());

        insta::assert_display_snapshot!(doc);
        insta::assert_debug_snapshot!(paths);
    }
}
