use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;

use apollo_compiler::ast;
use apollo_compiler::schema::FieldLookupError;
use tower::BoxError;

/// Transform a document with the given visitor.
pub(crate) fn document(
    visitor: &mut impl Visitor,
    document: &ast::Document,
) -> Result<ast::Document, BoxError> {
    let mut new = ast::Document {
        sources: document.sources.clone(),
        definitions: Vec::new(),
    };

    // go through the fragments and order them, starting with the ones that reference no other fragments
    // then the ones that depend only on the first one, and so on
    // This allows visitors like authorization to have all the required information if they encounter
    // a fragment spread while filtering a fragment
    let mut fragment_visitor = FragmentOrderVisitor::new();
    fragment_visitor.visit_document(document);
    let ordered_fragments = fragment_visitor.ordered_fragments();
    println!(
        "ordered_fragments: {:?}",
        ordered_fragments
            .iter()
            .map(|f| f.name.as_str())
            .collect::<Vec<_>>()
    );

    visitor.state().reset();

    // Then walk again through the fragments: if a fragment is entirely filtered, we want to
    // remove the spread too
    for def in ordered_fragments {
        visitor.state().used_fragments.clear();
        visitor.state().used_variables.clear();

        println!("fragment_definition, will look at {}", def.name.as_str());
        if let Some(new_def) = visitor.fragment_definition(def)? {
            // keep the list of used variables per fragment, as we need to use it to know which variables are used
            // in a query
            let used_variables = visitor.state().used_variables.clone();

            // keep the list of used fragments per fragment, as we need to use it to gather used variables later
            // unfortunately, we may not know the variable used for those fragments at this point, as they may not
            // have been processed yet
            let local_used_fragments = visitor.state().used_fragments.clone();

            visitor.state().defined_fragments.insert(
                def.name.as_str().to_string(),
                DefinedFragment {
                    fragment: new_def,
                    used_variables,
                    used_fragments: local_used_fragments,
                },
            );
        } else {
            println!("fragment {} is removed", def.name.as_str());
        }
    }

    // keeps the list of fragments used in the produced document (some fragment spreads might have been removed)
    let mut used_fragments = HashSet::new();

    for definition in &document.definitions {
        if let ast::Definition::OperationDefinition(def) = definition {
            let root_type = visitor
                .schema()
                .root_operation(def.operation_type)
                .ok_or("missing root operation definition")?
                .clone();

            // we reset the used_fragments and used_variables lists for each operation
            visitor.state().used_fragments.clear();
            visitor.state().used_variables.clear();
            if let Some(mut new_def) = visitor.operation(&root_type, def)? {
                let mut local_used_fragments = visitor.state().used_fragments.clone();

                // gather the entire list of fragments used in this operation
                loop {
                    let mut new_local_used_fragments = local_used_fragments.clone();
                    for fragment_name in local_used_fragments.iter() {
                        if let Some(defined_fragment) = visitor
                            .state()
                            .defined_fragments
                            .get(fragment_name.as_str())
                        {
                            new_local_used_fragments
                                .extend(defined_fragment.used_fragments.clone());
                        }
                    }

                    // no more changes, we can stop
                    if new_local_used_fragments.len() == local_used_fragments.len() {
                        break;
                    }
                    local_used_fragments = new_local_used_fragments;
                }

                // add to the list of used variables all the variables used in the fragment spreads
                for fragment_name in local_used_fragments.iter() {
                    if let Some(defined_fragment_used_variables) = visitor
                        .state()
                        .defined_fragments
                        .get(fragment_name.as_str())
                        .map(|defined_fragment| defined_fragment.used_variables.clone())
                    {
                        visitor
                            .state()
                            .used_variables
                            .extend(defined_fragment_used_variables);
                    }
                }
                used_fragments.extend(local_used_fragments);

                // remove unused variables
                new_def.variables.retain(|var| {
                    let res = visitor.state().used_variables.contains(var.name.as_str());
                    res
                });

                new.definitions
                    .push(ast::Definition::OperationDefinition(new_def.into()));
            }
        }
    }

    for (name, defined_fragment) in visitor.state().defined_fragments.clone().into_iter() {
        if used_fragments.contains(name.as_str()) {
            new.definitions.push(ast::Definition::FragmentDefinition(
                defined_fragment.fragment.into(),
            ));
        }
    }
    Ok(new)
}

/// Holds state during the transformation to account for used fragments and variables.
pub(crate) struct TransformState {
    used_fragments: HashSet<String>,
    used_variables: HashSet<String>,
    /// keeps the list of fragments defined in the produced document (the visitor might have removed some of them)
    defined_fragments: BTreeMap<String, DefinedFragment>,
}

#[derive(Clone)]
pub(crate) struct DefinedFragment {
    pub(crate) fragment: ast::FragmentDefinition,
    /// variables used in the fragment
    pub(crate) used_variables: HashSet<String>,
    /// fragments used in the fragment
    pub(crate) used_fragments: HashSet<String>,
}

impl TransformState {
    pub(crate) fn new() -> Self {
        Self {
            used_fragments: HashSet::new(),
            used_variables: HashSet::new(),
            defined_fragments: BTreeMap::new(),
        }
    }

    fn reset(&mut self) {
        self.used_fragments.clear();
        self.used_variables.clear();
        self.defined_fragments.clear();
    }

    pub(crate) fn fragments(&self) -> &BTreeMap<String, DefinedFragment> {
        &self.defined_fragments
    }
}

pub(crate) trait Visitor: Sized {
    fn schema(&self) -> &apollo_compiler::Schema;

    /// mutable state provided by the visitor to clean up unused fragments and variables
    /// do not modify directly
    fn state(&mut self) -> &mut TransformState;

    /// Transform an operation definition.
    ///
    /// Call the [`operation`] free function for the default behavior.
    /// Return `Ok(None)` to remove this operation.
    fn operation(
        &mut self,
        root_type: &str,
        def: &ast::OperationDefinition,
    ) -> Result<Option<ast::OperationDefinition>, BoxError> {
        operation(self, root_type, def)
    }

    /// Transform a fragment definition.
    ///
    /// Call the [`fragment_definition`] free function for the default behavior.
    /// Return `Ok(None)` to remove this fragment.
    fn fragment_definition(
        &mut self,
        def: &ast::FragmentDefinition,
    ) -> Result<Option<ast::FragmentDefinition>, BoxError> {
        fragment_definition(self, def)
    }

    /// Transform a field within a selection set.
    ///
    /// Call the [`field`] free function for the default behavior.
    /// Return `Ok(None)` to remove this field.
    fn field(
        &mut self,
        _parent_type: &str,
        field_def: &ast::FieldDefinition,
        def: &ast::Field,
    ) -> Result<Option<ast::Field>, BoxError> {
        field(self, field_def, def)
    }

    /// Transform a fragment spread within a selection set.
    ///
    /// Call the [`fragment_spread`] free function for the default behavior.
    /// Return `Ok(None)` to remove this fragment spread.
    fn fragment_spread(
        &mut self,
        def: &ast::FragmentSpread,
    ) -> Result<Option<ast::FragmentSpread>, BoxError> {
        let res = fragment_spread(self, def);
        if let Ok(Some(ref fragment)) = res.as_ref() {
            self.state()
                .used_fragments
                .insert(fragment.fragment_name.as_str().to_string());
        }
        res
    }

    /// Transform a inline fragment within a selection set.
    ///
    /// Call the [`inline_fragment`] free function for the default behavior.
    /// Return `Ok(None)` to remove this inline fragment.
    fn inline_fragment(
        &mut self,
        parent_type: &str,
        def: &ast::InlineFragment,
    ) -> Result<Option<ast::InlineFragment>, BoxError> {
        inline_fragment(self, parent_type, def)
    }
}

/// The default behavior for transforming an operation.
///
/// Never returns `Ok(None)`, the `Option` is for `Visitor` impl convenience.
pub(crate) fn operation(
    visitor: &mut impl Visitor,
    root_type: &str,
    def: &ast::OperationDefinition,
) -> Result<Option<ast::OperationDefinition>, BoxError> {
    let Some(selection_set) = selection_set(visitor, root_type, &def.selection_set)? else {
        return Ok(None);
    };

    Ok(Some(ast::OperationDefinition {
        name: def.name.clone(),
        operation_type: def.operation_type,
        variables: def.variables.clone(),
        directives: def.directives.clone(),
        selection_set,
    }))
}

/// The default behavior for transforming a fragment definition.
///
/// Never returns `Ok(None)`, the `Option` is for `Visitor` impl convenience.
pub(crate) fn fragment_definition(
    visitor: &mut impl Visitor,
    def: &ast::FragmentDefinition,
) -> Result<Option<ast::FragmentDefinition>, BoxError> {
    let Some(selection_set) = selection_set(visitor, &def.type_condition, &def.selection_set)?
    else {
        return Ok(None);
    };
    Ok(Some(ast::FragmentDefinition {
        name: def.name.clone(),
        type_condition: def.type_condition.clone(),
        directives: def.directives.clone(),
        selection_set,
    }))
}

/// The default behavior for transforming a field within a selection set.
///
/// Returns `Ok(None)` if the field had nested selections and they’re all removed.
pub(crate) fn field(
    visitor: &mut impl Visitor,
    field_def: &ast::FieldDefinition,
    def: &ast::Field,
) -> Result<Option<ast::Field>, BoxError> {
    let Some(selection_set) =
        selection_set(visitor, field_def.ty.inner_named_type(), &def.selection_set)?
    else {
        return Ok(None);
    };

    for argument in def.arguments.iter() {
        if let Some(var) = argument.value.as_variable() {
            visitor
                .state()
                .used_variables
                .insert(var.as_str().to_string());
        }
    }

    for directive in def.directives.iter() {
        for argument in directive.arguments.iter() {
            if let Some(var) = argument.value.as_variable() {
                visitor
                    .state()
                    .used_variables
                    .insert(var.as_str().to_string());
            }
        }
    }

    Ok(Some(ast::Field {
        alias: def.alias.clone(),
        name: def.name.clone(),
        arguments: def.arguments.clone(),
        directives: def.directives.clone(),
        selection_set,
    }))
}

/// The default behavior for transforming a fragment spread.
///
/// Never returns `Ok(None)`, the `Option` is for `Visitor` impl convenience.
pub(crate) fn fragment_spread(
    visitor: &mut impl Visitor,
    def: &ast::FragmentSpread,
) -> Result<Option<ast::FragmentSpread>, BoxError> {
    visitor
        .state()
        .used_fragments
        .insert(def.fragment_name.as_str().to_string());

    for directive in def.directives.iter() {
        for argument in directive.arguments.iter() {
            if let Some(var) = argument.value.as_variable() {
                visitor
                    .state()
                    .used_variables
                    .insert(var.as_str().to_string());
            }
        }
    }

    Ok(Some(def.clone()))
}

/// The default behavior for transforming an inline fragment.
///
/// Returns `Ok(None)` if all selections within the fragment are removed.
pub(crate) fn inline_fragment(
    visitor: &mut impl Visitor,
    parent_type: &str,
    def: &ast::InlineFragment,
) -> Result<Option<ast::InlineFragment>, BoxError> {
    let Some(selection_set) = selection_set(visitor, parent_type, &def.selection_set)? else {
        return Ok(None);
    };

    for directive in def.directives.iter() {
        for argument in directive.arguments.iter() {
            if let Some(var) = argument.value.as_variable() {
                visitor
                    .state()
                    .used_variables
                    .insert(var.as_str().to_string());
            }
        }
    }

    Ok(Some(ast::InlineFragment {
        type_condition: def.type_condition.clone(),
        directives: def.directives.clone(),
        selection_set,
    }))
}

pub(crate) fn selection_set(
    visitor: &mut impl Visitor,
    parent_type: &str,
    set: &[ast::Selection],
) -> Result<Option<Vec<ast::Selection>>, BoxError> {
    if set.is_empty() {
        return Ok(Some(Vec::new()));
    }
    let mut selections = Vec::new();
    for sel in set {
        match sel {
            ast::Selection::Field(def) => {
                let field_def = visitor
                    .schema()
                    .type_field(parent_type, &def.name)
                    .map_err(|e| match e {
                        FieldLookupError::NoSuchType => format!("type `{parent_type}` not defined"),
                        FieldLookupError::NoSuchField(_, _) => {
                            format!("no field `{}` in type `{parent_type}`", &def.name)
                        }
                    })?
                    .clone();
                if let Some(sel) = visitor.field(parent_type, &field_def, def)? {
                    selections.push(ast::Selection::Field(sel.into()))
                }
            }
            ast::Selection::FragmentSpread(def) => {
                if let Some(sel) = visitor.fragment_spread(def)? {
                    selections.push(ast::Selection::FragmentSpread(sel.into()))
                }
            }
            ast::Selection::InlineFragment(def) => {
                let fragment_type = def
                    .type_condition
                    .as_ref()
                    .map(|s| s.as_str())
                    .unwrap_or(parent_type);
                if let Some(sel) = visitor.inline_fragment(fragment_type, def)? {
                    selections.push(ast::Selection::InlineFragment(sel.into()))
                }
            }
        }
    }
    Ok((!selections.is_empty()).then_some(selections))
}

/// this visitor goes through the list of fragments in the query, looking at fragment spreads
/// in their selection, and generates a list of fragments in the order they should be visited
/// by the transform visitor, to ensure a fragment has already been visited before it is
/// referenced in a fragment spread
struct FragmentOrderVisitor<'a> {
    // the resulting list of ordered fragments
    ordered_fragments: Vec<String>,
    // list of fragments in the document
    fragments: HashMap<String, &'a apollo_compiler::ast::FragmentDefinition>,

    // fragment dependencies. The key is a fragment name, the value is all the fragments that reference it
    // in a fragment spread
    dependencies: HashMap<String, Vec<String>>,
    // name of the fragment currently being visited
    current: Option<String>,

    // how many fragments are used by each fragment. This is decremented when a referenced fragment
    // is added to the final list. Once it reaches 0, the fragment is added to the final list too
    rank: HashMap<String, usize>,
}

impl<'a> FragmentOrderVisitor<'a> {
    fn new() -> Self {
        Self {
            ordered_fragments: Vec::new(),
            fragments: HashMap::new(),
            dependencies: HashMap::new(),
            current: None,
            rank: HashMap::new(),
        }
    }

    fn rerank(&mut self, name: &str) {
        if let Some(v) = self.dependencies.remove(name) {
            for dep in v {
                if let Some(rank) = self.rank.get_mut(&dep) {
                    *rank -= 1;
                    if *rank == 0 {
                        self.ordered_fragments.push(dep.clone());
                        self.rerank(&dep);
                    }
                }
            }
        }
    }

    fn ordered_fragments(self) -> Vec<&'a ast::FragmentDefinition> {
        let mut ordered_fragments = Vec::new();
        for name in self.ordered_fragments {
            ordered_fragments.push(*self.fragments.get(name.as_str()).unwrap());
        }
        ordered_fragments
    }

    fn visit_document(&mut self, doc: &'a ast::Document) {
        for definition in &doc.definitions {
            if let ast::Definition::FragmentDefinition(def) = definition {
                self.visit_fragment_definition(&def);
            }
        }
    }

    fn visit_fragment_definition(&mut self, def: &'a ast::FragmentDefinition) {
        let name = def.name.as_str().to_string();
        self.fragments.insert(name.clone(), def);

        self.current = Some(name.clone());
        self.rank.insert(name.clone(), 0);

        self.visit_selection_set(&def.selection_set);

        if self.rank.get(&name).unwrap() == &0 {
            // if the fragment does not reference any other fragments, it is ready to be added to the final list
            self.ordered_fragments.push(name.clone());
            // then we rerank all the fragments that reference this one: if any of them reaches the rank 0, they
            // are added to the final list too
            self.rerank(&name);
        }
        println!(
            "visited fragment definition for {}, ordered fragments are now: {:?}, ranks: {:?}, dependencies: {:?}",
            name, self.ordered_fragments, self.rank, self.dependencies
        );
    }

    fn visit_selection_set(&mut self, selection_set: &[apollo_compiler::ast::Selection]) {
        for selection in selection_set {
            match selection {
                ast::Selection::Field(def) => self.visit_selection_set(&def.selection_set),
                ast::Selection::InlineFragment(def) => self.visit_selection_set(&def.selection_set),
                ast::Selection::FragmentSpread(def) => {
                    let name = def.fragment_name.as_str().to_string();

                    println!("fragment {:?} depends on {:?}", self.current, name);

                    // we have already seen this fragment, so we don't need to add it again
                    if self.rank.get(name.as_str()) == Some(&0) {
                        continue;
                    }
                    if let Some(current) = self.current.as_ref() {
                        if let Some(rank) = self.rank.get_mut(current.as_str()) {
                            *rank += 1;
                        }
                        if !self.dependencies.contains_key(&name) {
                            self.dependencies.insert(name, vec![current.clone()]);
                        } else {
                            self.dependencies
                                .get_mut(&name)
                                .expect("membership was just checked")
                                .push(current.clone());
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_add_directive_to_fields() {
        struct AddDirective {
            schema: apollo_compiler::Schema,
            state: TransformState,
        }

        impl Visitor for AddDirective {
            fn field(
                &mut self,
                _parent_type: &str,
                field_def: &ast::FieldDefinition,
                def: &ast::Field,
            ) -> Result<Option<ast::Field>, BoxError> {
                Ok(field(self, field_def, def)?.map(|mut new| {
                    new.directives.push(
                        ast::Directive {
                            name: apollo_compiler::name!("added"),
                            arguments: Vec::new(),
                        }
                        .into(),
                    );
                    new
                }))
            }

            fn schema(&self) -> &apollo_compiler::Schema {
                &self.schema
            }

            fn state(&mut self) -> &mut TransformState {
                &mut self.state
            }
        }

        let graphql = "
        type Query {
            a(id: ID): String
            b: Int
            next: Query
        }
        directive @defer(label: String, if: Boolean! = true) on FRAGMENT_SPREAD | INLINE_FRAGMENT

        query($id: ID = null) {
            a(id: $id)
            ... @defer {
                b
            }
            ... F
        }

        fragment F on Query {
            next {
                a
            }
        }
    ";
        let ast = apollo_compiler::ast::Document::parse(graphql, "").unwrap();
        let (schema, _doc) = ast.to_mixed_validate().unwrap();
        let schema = schema.into_inner();
        let mut visitor = AddDirective {
            schema,
            state: TransformState::new(),
        };
        let expected = "query($id: ID = null) {
  a(id: $id) @added
  ... @defer {
    b @added
  }
  ...F
}

fragment F on Query {
  next @added {
    a @added
  }
}
";
        assert_eq!(document(&mut visitor, &ast).unwrap().to_string(), expected)
    }

    struct RemoveDirective {
        schema: apollo_compiler::Schema,
        state: TransformState,
    }

    impl RemoveDirective {
        fn new(schema: apollo_compiler::Schema) -> Self {
            Self {
                schema,
                state: TransformState::new(),
            }
        }
    }

    impl Visitor for RemoveDirective {
        fn field(
            &mut self,
            _parent_type: &str,
            field_def: &ast::FieldDefinition,
            def: &ast::Field,
        ) -> Result<Option<ast::Field>, BoxError> {
            if def.directives.iter().any(|d| d.name == "remove") {
                return Ok(None);
            }
            field(self, field_def, def)
        }

        fn fragment_spread(
            &mut self,
            def: &ast::FragmentSpread,
        ) -> Result<Option<ast::FragmentSpread>, BoxError> {
            if def.directives.iter().any(|d| d.name == "remove") {
                return Ok(None);
            }

            // remove the fragment spread if the fragment was removed
            if !self
                .state()
                .fragments()
                .contains_key(def.fragment_name.as_str())
            {
                return Ok(None);
            }

            fragment_spread(self, def)
        }

        fn inline_fragment(
            &mut self,
            _parent_type: &str,
            def: &ast::InlineFragment,
        ) -> Result<Option<ast::InlineFragment>, BoxError> {
            if def.directives.iter().any(|d| d.name == "remove") {
                return Ok(None);
            }
            inline_fragment(self, _parent_type, def)
        }

        fn schema(&self) -> &apollo_compiler::Schema {
            &self.schema
        }

        fn state(&mut self) -> &mut TransformState {
            &mut self.state
        }
    }

    struct TestResult<'a> {
        query: &'a str,
        result: ast::Document,
    }

    impl<'a> std::fmt::Display for TestResult<'a> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "query:\n{}\nfiltered:\n{}", self.query, self.result,)
        }
    }

    static TRANSFORM_REMOVE_SCHEMA: &str = r#"
    schema
      @link(url: "https://specs.apollo.dev/link/v1.0")
      @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION)
      @link(url: "https://specs.apollo.dev/authenticated/v0.1", for: SECURITY)
    {
      query: Query
    }
    directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
    directive @remove on FIELD | INLINE_FRAGMENT | FRAGMENT_SPREAD
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

    type Query  {
        a(arg: String): String
        b: Obj
        c: Int
    }

    type Obj {
        a: String
    }
    "#;

    #[test]
    fn remove_directive() {
        let ast = apollo_compiler::ast::Document::parse(TRANSFORM_REMOVE_SCHEMA, "").unwrap();
        let (schema, _doc) = ast.to_mixed_validate().unwrap();
        let schema = schema.into_inner();
        let mut visitor = RemoveDirective::new(schema.clone());

        // test removed fragment
        let query = r#"
            query {
                a
               ... F @remove
            }

            fragment F on Query {
                b {
                    a
                }
            }"#;
        let doc = ast::Document::parse(query, "query.graphql").unwrap();
        let result = document(&mut visitor, &doc).unwrap();
        insta::assert_snapshot!(TestResult { query, result });

        // test removed field with variable
        let query = r#"
            query($a: String) {
                a(arg: $a) @remove
                c
            }"#;
        let doc = ast::Document::parse(query, "query.graphql").unwrap();
        let result = document(&mut visitor, &doc).unwrap();
        insta::assert_snapshot!(TestResult { query, result });

        // test removed field with variable in fragment
        let query = r#"
            query($a: String) {
                ... F
                c
            }

            fragment F on Query {
                a(arg: $a) @remove
            }"#;
        let doc = ast::Document::parse(query, "query.graphql").unwrap();
        let result = document(&mut visitor, &doc).unwrap();
        insta::assert_snapshot!(TestResult { query, result });

        // test field with variable in removed fragment
        let query = r#"
            query($a: String) {
                ... F @remove
                c
            }

            fragment F on Query {
                a(arg: $a)
            }"#;
        let doc = ast::Document::parse(query, "query.graphql").unwrap();
        let result = document(&mut visitor, &doc).unwrap();
        insta::assert_snapshot!(TestResult { query, result });

        // test field with variable in fragment nested in removed fragment
        let query = r#"
            query($a: String) {
                ... F @remove
                c
            }

            fragment F on Query {
                ... G
            }

            fragment G on Query {
                a(arg: $a)
            }
            "#;
        let doc = ast::Document::parse(query, "query.graphql").unwrap();
        let result = document(&mut visitor, &doc).unwrap();
        insta::assert_snapshot!(TestResult { query, result });

        // test removed field with variable in fragment nested in fragment
        let query = r#"
            query($a: String) {
                ... F
                c
            }

            fragment F on Query {
                ... G
            }

            fragment G on Query {
                a(arg: $a) @remove
            }
            "#;
        let doc = ast::Document::parse(query, "query.graphql").unwrap();
        let result = document(&mut visitor, &doc).unwrap();
        insta::assert_snapshot!(TestResult { query, result });
    }
}
