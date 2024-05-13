//! Valid federation 2 subgraphs.
//!
//! Note: technically, federation 1 subgraphs are still accepted as input of
//! composition. However, there is some pre-composition steps that "massage"
//! the input schema to transform them in fully valid federation 2 subgraphs,
//! so the subgraphs seen by composition and query planning are always fully
//! valid federation 2 ones, and this is what this database handles.
//! Note2: This does assumes that whichever way an implementation of this
//! trait is created, some validation that the underlying schema is a valid
//! federation subgraph (so valid graphql, link to the federation spec, and
//! pass additional federation validations). If this is not the case, most
//! of the methods here will panic.

use std::sync::Arc;

use apollo_compiler::executable::Directive;
use apollo_compiler::executable::SelectionSet;
use apollo_compiler::name;
use apollo_compiler::schema::Name;
use apollo_compiler::Schema;

use crate::link::database::links_metadata;
use crate::link::spec::Identity;
use crate::link::spec::APOLLO_SPEC_DOMAIN;
use crate::link::Link;

// TODO: we should define this as part as some more generic "FederationSpec" definition, but need
// to define the ground work for that in `apollo-at-link` first.
pub fn federation_link_identity() -> Identity {
    Identity {
        domain: APOLLO_SPEC_DOMAIN.to_string(),
        name: name!("federation"),
    }
}

#[derive(Eq, PartialEq, Debug, Clone)]
pub struct Key {
    pub type_name: Name,
    // TODO: this should _not_ be an Option below; but we don't know how to build the SelectionSet,
    // so until we have a solution, we use None to have code that compiles.
    selections: Option<Arc<SelectionSet>>,
}

impl Key {
    // TODO: same remark as above: not meant to be `Option`
    // TODO remove suppression OR use method in final version
    #[allow(dead_code)]
    pub fn selections(&self) -> Option<Arc<SelectionSet>> {
        self.selections.clone()
    }

    pub(crate) fn from_directive_application(
        type_name: &Name,
        directive: &Directive,
    ) -> Option<Key> {
        directive
            .arguments
            .iter()
            .find(|arg| arg.name == "fields")
            .and_then(|arg| arg.value.as_str())
            .map(|_value| Key {
                type_name: type_name.clone(),
                // TODO: obviously not what we want.
                selections: None,
            })
    }
}

pub fn federation_link(schema: &Schema) -> Arc<Link> {
    links_metadata(schema)
        // TODO: error handling?
        .unwrap_or_default()
        .unwrap_or_default()
        .for_identity(&federation_link_identity())
        .expect("The presence of the federation link should have been validated on construction")
}

/// The name of the @key directive in this subgraph.
/// This will either return 'federation__key' if the `@key` directive is not imported,
/// or whatever never it is imported under otherwise. Commonly, this would just be `key`.
pub fn key_directive_name(schema: &Schema) -> Name {
    federation_link(schema).directive_name_in_schema(&name!("key"))
}

pub fn keys(schema: &Schema, type_name: &Name) -> Vec<Key> {
    let key_name = key_directive_name(schema);
    if let Some(type_def) = schema.types.get(type_name) {
        type_def
            .directives()
            .get_all(&key_name)
            .filter_map(|directive| Key::from_directive_application(type_name, directive))
            .collect()
    } else {
        vec![]
    }
}
