use std::fmt;
use std::fmt::Formatter;

use apollo_compiler::ast::Directive;
use apollo_compiler::collections::HashMap;
use apollo_compiler::executable::FieldSet;
use apollo_compiler::validation::Valid;
use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use itertools::Itertools;

use super::compare_keys::field_set_is_subset;
use super::compare_keys::optimize_field_set;
use super::VariableReference;
use super::FEDERATION_FIELDS_ARGUMENT_NAME;
use crate::sources::connect::validation::Code;
use crate::sources::connect::validation::Message;
use crate::sources::connect::EntityResolver;
use crate::sources::connect::Namespace;

/// Collects keys and entity connectors for comparison and validation.
#[derive(Default)]
pub(crate) struct EntityKeyChecker<'schema> {
    /// Any time we see `type T @key(fields: "f")` (with resolvable: true)
    resolvable_keys: Vec<(Valid<FieldSet>, &'schema Node<Directive>, &'schema Name)>,
    /// Any time we see either:
    /// - `type Query { t(f: X): T @connect(entity: true) }` (Explicit entity resolver)
    /// - `type T { f: X g: Y @connect(... $this.f ...) }`  (Implicit entity resolver)
    entity_connectors: HashMap<Name, Vec<Valid<FieldSet>>>,
}

impl<'schema> EntityKeyChecker<'schema> {
    pub(crate) fn add_key(
        &mut self,
        field_set: &FieldSet,
        directive: &'schema Node<Directive>,
        type_name: &'schema Name,
    ) {
        self.resolvable_keys
            .push((optimize_field_set(field_set), directive, type_name));
    }

    pub(crate) fn add_connector(&mut self, field_set: &Valid<FieldSet>) {
        self.entity_connectors
            .entry(field_set.selection_set.ty.clone())
            .or_default()
            .push(optimize_field_set(field_set));
    }

    /// For each @key we've seen, check if there's a corresponding entity connector
    /// by semantically comparing the @key field set with the synthesized field set
    /// from the connector's arguments.
    ///
    /// The comparison is done by checking if the @key field set is a subset of the
    /// entity connector's field set. It's not equality because we convert `@external`/
    /// `@requires` fields to keys for simplicity's sake.
    pub(crate) fn check_for_missing_entity_connectors(&self, schema: &Schema) -> Vec<Message> {
        let mut messages = Vec::new();

        for (key, directive, type_name) in &self.resolvable_keys {
            let for_type = self.entity_connectors.get(&key.selection_set.ty);
            let key_exists = for_type
                .map(|connectors| {
                    connectors
                        .iter()
                        .any(|connector| field_set_is_subset(key, connector))
                })
                .unwrap_or(false);
            if !key_exists {
                messages.push(Message {
                    code: Code::MissingEntityConnector,
                    message: format!(
                        "Entity resolution for `@key(fields: \"{}\")` on `{}` is not implemented by a connector. See https://go.apollo.dev/connectors/directives/#rules-for-entity-true",
                        directive.argument_by_name(&FEDERATION_FIELDS_ARGUMENT_NAME, schema).ok().and_then(|arg| arg.as_str()).unwrap_or_default(),
                        type_name,
                    ),
                    locations: directive
                        .line_column_range(&schema.sources)
                        .into_iter()
                        .collect(),
                });
            }
        }

        messages
    }
}

impl std::fmt::Debug for EntityKeyChecker<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("EntityKeyChecker")
            .field(
                "resolvable_keys",
                &self
                    .resolvable_keys
                    .iter()
                    .map(|(fs, _, _)| {
                        format!(
                            "... on {} {}",
                            fs.selection_set.ty,
                            fs.selection_set.serialize().no_indent()
                        )
                    })
                    .collect_vec(),
            )
            .field(
                "entity_connectors",
                &self
                    .entity_connectors
                    .values()
                    .flatten()
                    .map(|fs| {
                        format!(
                            "... on {} {}",
                            fs.selection_set.ty,
                            fs.selection_set.serialize().no_indent()
                        )
                    })
                    .collect_vec(),
            )
            .finish()
    }
}

/// Given the parts of the connector that contain variables, synthesize a field
/// set appropriate for use in a @key directive.
///
/// This only looks at `$args` variables because those are the ones used
/// for `entity: true` connectors.
///
/// TODO: this is simpler to code in expand/mod.rs and might be something we
/// could combine. The expand code not only generates a key, but also copies
/// the types related to the key into the expanded subgraph, so it does more work
/// than this function.
pub(crate) fn make_key_field_set_from_variables(
    schema: &Schema,
    object_type_name: &Name,
    variables: &[VariableReference<Namespace>],
    resolver: EntityResolver,
) -> Result<Option<Valid<FieldSet>>, Message> {
    let params = variables
        .iter()
        .filter(|var| match resolver {
            EntityResolver::Explicit => var.namespace.namespace == Namespace::Args,
            EntityResolver::Implicit => var.namespace.namespace == Namespace::This,
        })
        .unique()
        .collect_vec();

    if params.is_empty() {
        return Ok(None);
    }

    let mut keys = Vec::with_capacity(params.len());
    for VariableReference { path, .. } in params {
        let parts = path.iter().map(|part| part.part.as_ref());
        let field_and_selection = FieldAndSelection::from_path(parts);
        keys.push(field_and_selection.to_key());
    }

    FieldSet::parse_and_validate(
        Valid::assume_valid_ref(schema),
        object_type_name.clone(),
        keys.join(" "),
        "",
    )
    // This shouldn't happen because we've already validated the inputs using ArgumentVisitor
    .map_err(|_| Message {
        code: Code::GraphQLError,
        message: format!("Variables used in connector (`{}`) for `{}` cannot be used to create a valid `@key` directive.", variables.iter().join("`, `"), object_type_name),
        locations: vec![],
    }).map(Some)
}

/// Represents a field and the subselection of that field, which can then be joined together into
/// a full named selection (which is the same as a key, in simple cases).
///
/// TODO: this is copied from expand/mod.rs
struct FieldAndSelection<'a> {
    field_name: &'a str,
    sub_selection: String,
}

impl<'a> FieldAndSelection<'a> {
    /// Extract from a path like `a.b.c` into `a` and `b { c }`
    fn from_path<I: IntoIterator<Item = &'a str>>(parts: I) -> Self {
        let mut parts = parts.into_iter().peekable();
        let field_name = parts.next().unwrap_or_default();
        let mut sub_selection = String::new();
        let mut closing_braces = 0;
        while let Some(sub_path) = parts.next() {
            sub_selection.push_str(sub_path);
            if parts.peek().is_some() {
                sub_selection.push_str(" { ");
                closing_braces += 1;
            }
        }
        for _ in 0..closing_braces {
            sub_selection.push_str(" }");
        }
        FieldAndSelection {
            field_name,
            sub_selection,
        }
    }

    fn to_key(&self) -> String {
        if self.sub_selection.is_empty() {
            self.field_name.to_string()
        } else {
            format!("{} {{ {} }}", self.field_name, self.sub_selection)
        }
    }
}
