//! Deriving `@key` resolvability of a source schema from its lookups.
//!
//! A source schema has no `_entities`: it resolves entities through its `@lookup` fields. So a
//! key some lookup recalls the entity by is resolvable, and any other declared key is identity
//! only and gets `resolvable: false`. A key that is only implied by a lookup's arguments is added,
//! since the specification lets it be omitted. This runs before `@internal` elements are removed
//! (see [`super::internal`]), as internal lookups still resolve entities.

use std::sync::Arc;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use apollo_compiler::ast;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::name;
use apollo_compiler::schema::ExtendedType;

use super::CompositeNames;
use super::lookups::collect_lookups;
use crate::bail;
use crate::error::FederationError;
use crate::error::MultipleFederationErrors;
use crate::error::SingleFederationError;
use crate::operation::FieldSetDisplay;
use crate::operation::SelectionSet;
use crate::schema::ValidFederationSchema;
use crate::schema::field_selection_map::validate::possible_types;
use crate::schema::field_set::parse_field_set;

/// Keys as parsed field sets, which compare equal however their fields are ordered.
type Keys = IndexSet<Arc<SelectionSet>>;

/// The `@key` applications of `schema` rewritten to match its lookups: for each object and
/// interface type whose keys change, its complete new directive list.
pub(crate) fn derive_key_resolvability(
    schema: &ValidFederationSchema,
    names: &CompositeNames,
) -> Result<IndexMap<Name, ast::DirectiveList>, FederationError> {
    let keys_by_type = recalled_keys_by_type(schema, names)?;
    let mut rewritten = IndexMap::default();
    for (type_name, ty) in &schema.schema().types {
        let Some(recalled) = recalled_keys(schema.schema(), type_name, ty, &keys_by_type) else {
            continue;
        };
        if let Some(directives) = rewrite_keys(schema, type_name, ty, &recalled, names)? {
            rewritten.insert(type_name.clone(), directives);
        }
    }
    Ok(rewritten)
}

/// Replace the directives of the types [`derive_key_resolvability`] rewrote.
pub(crate) fn apply_rewritten_keys(
    schema: &mut Schema,
    rewritten: IndexMap<Name, ast::DirectiveList>,
) {
    for (type_name, directives) in rewritten {
        match schema.types.get_mut(&type_name) {
            Some(ExtendedType::Object(object)) => object.make_mut().directives = directives,
            Some(ExtendedType::Interface(interface)) => {
                interface.make_mut().directives = directives
            }
            _ => {}
        }
    }
}

/// The keys each concrete type is recalled by. Internal lookups count: they are removed from the
/// merged schema, but still resolve entities.
fn recalled_keys_by_type(
    schema: &ValidFederationSchema,
    names: &CompositeNames,
) -> Result<IndexMap<Name, Keys>, FederationError> {
    let mut texts: IndexSet<(Name, String)> = IndexSet::default();
    for lookup in collect_lookups(schema.schema(), &names.lookup, &names.is) {
        texts.extend(
            lookup
                .keys(schema.schema())
                .into_iter()
                .map(|key| (key.concrete_type, key.key)),
        );
    }
    let mut keys_by_type: IndexMap<Name, Keys> = IndexMap::default();
    let mut errors = Vec::new();
    for (type_name, text) in texts {
        match parse_field_set(schema, type_name.clone(), &text, true) {
            Ok(key) => {
                keys_by_type
                    .entry(type_name)
                    .or_default()
                    .insert(Arc::new(key));
            }
            Err(_) => errors.push(SingleFederationError::KeyInvalidFields {
                application: format!("@key(fields: \"{text}\")"),
                message: format!(
                    "the key implied by a @lookup field is not a valid field set on \
                     \"{type_name}\" in this source schema; declare the key fields on \
                     \"{type_name}\" (as @external if another source schema resolves them)"
                ),
                target_type: type_name,
            }),
        }
    }
    if !errors.is_empty() {
        return Err(MultipleFederationErrors { errors }.into());
    }
    Ok(keys_by_type)
}

/// The keys `ty` is recalled by, or `None` when it cannot have keys. An interface is recalled by
/// a key only when every implementation is.
fn recalled_keys(
    schema: &Schema,
    type_name: &Name,
    ty: &ExtendedType,
    keys_by_type: &IndexMap<Name, Keys>,
) -> Option<Keys> {
    match ty {
        ExtendedType::Object(_) => Some(keys_by_type.get(type_name).cloned().unwrap_or_default()),
        ExtendedType::Interface(_) => {
            let mut implementations = possible_types(schema, type_name).into_iter();
            let Some(first) = implementations.next() else {
                return Some(Keys::default());
            };
            let mut common = keys_by_type.get(&first).cloned().unwrap_or_default();
            for implementation in implementations {
                let keys = keys_by_type.get(&implementation);
                common.retain(|key| keys.is_some_and(|keys| keys.contains(key)));
            }
            Some(common)
        }
        _ => None,
    }
}

/// The directives of `ty` with every declared key that is not `recalled` made unresolvable and,
/// on an object type, every `recalled` key that is not declared added. `None` when nothing
/// changes.
fn rewrite_keys(
    schema: &ValidFederationSchema,
    type_name: &Name,
    ty: &ExtendedType,
    recalled: &Keys,
    names: &CompositeNames,
) -> Result<Option<ast::DirectiveList>, FederationError> {
    let mut directives = ty.directives().clone();
    let mut declared = Keys::default();
    let mut changed = false;
    for directive in directives.iter_mut() {
        if directive.name != names.key {
            continue;
        }
        let Some(fields) = directive
            .specified_argument_by_name("fields")
            .and_then(|v| v.as_str())
        else {
            continue;
        };
        // This should never fail: subgraph validation already ran and rejects invalid keys.
        let Ok(key) = parse_field_set(schema, type_name.clone(), fields, true) else {
            bail!("@key(fields: \"{fields}\") on \"{type_name}\" is not a valid field set");
        };
        let key = Arc::new(key);
        if !recalled.contains(&key) {
            make_unresolvable(directive);
            changed = true;
        }
        declared.insert(key);
    }
    if matches!(ty, ExtendedType::Object(_)) {
        for key in recalled.iter().filter(|key| !declared.contains(*key)) {
            directives.push(Node::new(ast::Directive {
                name: names.key.clone(),
                arguments: vec![Node::new(ast::Argument {
                    name: name!("fields"),
                    value: Node::new(ast::Value::String(FieldSetDisplay(key).to_string())),
                })],
            }));
            changed = true;
        }
    }
    Ok(changed.then_some(directives))
}

/// Set `resolvable: false` on a `@key` application, replacing any `resolvable` it has.
fn make_unresolvable(key: &mut Node<ast::Directive>) {
    let key = key.make_mut();
    key.arguments
        .retain(|argument| argument.name != "resolvable");
    key.arguments.push(Node::new(ast::Argument {
        name: name!("resolvable"),
        value: Node::new(ast::Value::Boolean(false)),
    }));
}
