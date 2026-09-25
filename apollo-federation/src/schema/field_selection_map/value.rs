//! Analysis of a `FieldSelectionMap`: which output fields it reads for a given concrete type.
//!
//! This is what turns a lookup's arguments into a _stable key_ (a field set on the entity type),
//! and what a `@require` map needs fetched before its argument can be materialized.

use std::fmt;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use apollo_compiler::ast;
use apollo_compiler::collections::IndexMap;

use super::Path;
use super::SelectedListValue;
use super::SelectedObjectField;
use super::SelectedObjectValue;
use super::SelectedValue;
use super::SelectedValueEntry;
use super::validate::possible_types;

/// A tree of output field selections, printable as a field set (`id address { id }`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SelectionTree {
    /// Keyed by the printed field (name plus arguments).
    fields: IndexMap<String, (Name, Vec<Node<ast::Argument>>, SelectionTree)>,
    /// Inline fragments, keyed by type condition.
    fragments: IndexMap<Name, SelectionTree>,
}

impl SelectionTree {
    pub(crate) fn is_empty(&self) -> bool {
        self.fields.is_empty() && self.fragments.is_empty()
    }

    fn field(&mut self, name: &Name, arguments: &[Node<ast::Argument>]) -> &mut SelectionTree {
        let mut key = name.to_string();
        if !arguments.is_empty() {
            key.push('(');
            for (i, argument) in arguments.iter().enumerate() {
                if i > 0 {
                    key.push_str(", ");
                }
                key.push_str(&format!(
                    "{}: {}",
                    argument.name,
                    argument.value.serialize().no_indent()
                ));
            }
            key.push(')');
        }
        &mut self
            .fields
            .entry(key)
            .or_insert_with(|| (name.clone(), arguments.to_vec(), SelectionTree::default()))
            .2
    }

    fn fragment(&mut self, type_condition: &Name) -> &mut SelectionTree {
        self.fragments.entry(type_condition.clone()).or_default()
    }

    pub(crate) fn merge(&mut self, other: &SelectionTree) {
        for (key, (name, arguments, subtree)) in &other.fields {
            self.fields
                .entry(key.clone())
                .or_insert_with(|| (name.clone(), arguments.clone(), SelectionTree::default()))
                .2
                .merge(subtree);
        }
        for (type_condition, subtree) in &other.fragments {
            self.fragment(type_condition).merge(subtree);
        }
    }

    /// A copy with fields and fragments sorted, so that equal selections print equally.
    pub(crate) fn canonical(&self) -> SelectionTree {
        let mut fields: Vec<_> = self
            .fields
            .iter()
            .map(|(key, (name, arguments, subtree))| {
                (
                    key.clone(),
                    (name.clone(), arguments.clone(), subtree.canonical()),
                )
            })
            .collect();
        fields.sort_by(|a, b| a.0.cmp(&b.0));
        let mut fragments: Vec<_> = self
            .fragments
            .iter()
            .map(|(name, subtree)| (name.clone(), subtree.canonical()))
            .collect();
        fragments.sort_by(|a, b| a.0.cmp(&b.0));
        SelectionTree {
            fields: fields.into_iter().collect(),
            fragments: fragments.into_iter().collect(),
        }
    }

    /// Build a tree from a parsed field set (`@key(fields:)`).
    pub(crate) fn from_selection_set(
        selection_set: &apollo_compiler::executable::SelectionSet,
    ) -> Self {
        use apollo_compiler::executable::Selection;
        let mut tree = SelectionTree::default();
        for selection in &selection_set.selections {
            match selection {
                Selection::Field(field) => {
                    let arguments: Vec<Node<ast::Argument>> = field.arguments.clone();
                    tree.field(&field.name, &arguments)
                        .merge(&Self::from_selection_set(&field.selection_set));
                }
                Selection::InlineFragment(fragment) => {
                    let subtree = Self::from_selection_set(&fragment.selection_set);
                    match &fragment.type_condition {
                        Some(type_condition) => tree.fragment(type_condition).merge(&subtree),
                        None => tree.merge(&subtree),
                    }
                }
                Selection::FragmentSpread(_) => {}
            }
        }
        tree
    }

    /// The top-level field names, in selection order.
    pub(crate) fn top_level_fields(&self) -> impl Iterator<Item = &Name> {
        self.fields.values().map(|(name, _, _)| name)
    }
}

impl fmt::Display for SelectionTree {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        for (key, (_, _, subtree)) in &self.fields {
            if !first {
                f.write_str(" ")?;
            }
            first = false;
            f.write_str(key)?;
            if !subtree.is_empty() {
                write!(f, " {{ {subtree} }}")?;
            }
        }
        for (type_condition, subtree) in &self.fragments {
            if !first {
                f.write_str(" ")?;
            }
            first = false;
            write!(f, "... on {type_condition} {{ {subtree} }}")?;
        }
        Ok(())
    }
}

/// Collects selections for one concrete root type.
struct Collector<'a> {
    schema: &'a Schema,
    concrete: &'a Name,
}

impl Collector<'_> {
    fn admits(&self, type_condition: &Name) -> bool {
        possible_types(self.schema, type_condition).contains(self.concrete)
    }

    /// Collect the first applicable alternative of `value`. `at_root` is whether `value` is scoped
    /// to the concrete root type (type conditions there select alternatives); elsewhere every
    /// alternative's fields are collected, since the nested runtime type is unknown statically.
    fn value(&self, value: &SelectedValue, at_root: bool, tree: &mut SelectionTree) -> bool {
        if at_root {
            for entry in &value.alternatives {
                let mut candidate = SelectionTree::default();
                if self.entry(entry, true, &mut candidate) {
                    tree.merge(&candidate);
                    return true;
                }
            }
            false
        } else {
            for entry in &value.alternatives {
                self.entry(entry, false, tree);
            }
            true
        }
    }

    fn entry(&self, entry: &SelectedValueEntry, at_root: bool, tree: &mut SelectionTree) -> bool {
        match entry {
            SelectedValueEntry::Path(path) => self.path(path, at_root, tree).is_some(),
            SelectedValueEntry::PathObject(path, object) => match self.path(path, at_root, tree) {
                Some(leaf) => {
                    self.object(object, false, leaf);
                    true
                }
                None => false,
            },
            SelectedValueEntry::PathList(path, list) => match self.path(path, at_root, tree) {
                Some(leaf) => {
                    self.list(list, leaf);
                    true
                }
                None => false,
            },
            SelectedValueEntry::Object(object) => self.object(object, at_root, tree),
        }
    }

    /// Add a path to `tree`, returning the subtree of its last segment, or `None` if a root-level
    /// type condition excludes the concrete type.
    fn path<'t>(
        &self,
        path: &Path,
        at_root: bool,
        tree: &'t mut SelectionTree,
    ) -> Option<&'t mut SelectionTree> {
        let mut current = tree;
        if let Some(type_condition) = &path.type_condition {
            if at_root {
                if !self.admits(type_condition) {
                    return None;
                }
            } else {
                current = current.fragment(type_condition);
            }
        }
        for segment in &path.segments {
            current = current.field(&segment.field, &segment.arguments);
            if let Some(type_condition) = &segment.type_condition {
                current = current.fragment(type_condition);
            }
        }
        Some(current)
    }

    fn object(
        &self,
        object: &SelectedObjectValue,
        at_root: bool,
        tree: &mut SelectionTree,
    ) -> bool {
        let mut candidate = SelectionTree::default();
        for field in &object.fields {
            match field {
                SelectedObjectField::Labeled(_, value) => {
                    if !self.value(value, at_root, &mut candidate) {
                        return false;
                    }
                }
                SelectedObjectField::Shorthand(name, arguments) => {
                    candidate.field(name, arguments);
                }
            }
        }
        tree.merge(&candidate);
        true
    }

    fn list(&self, list: &SelectedListValue, tree: &mut SelectionTree) {
        match list {
            SelectedListValue::Value(value) => {
                self.value(value, false, tree);
            }
            SelectedListValue::List(inner) => self.list(inner, tree),
        }
    }
}

/// The output fields `value` reads when the root object has the concrete type `concrete`, one
/// tree per applicable top-level alternative (with the alternative's index). Empty when no
/// alternative applies.
///
/// Each top-level alternative of an argument is a distinct way to recall the entity (for a
/// `@oneOf` input, a distinct _stable key_), hence one tree each.
pub(crate) fn selections_for_type(
    value: &SelectedValue,
    schema: &Schema,
    concrete: &Name,
) -> Vec<(usize, SelectionTree)> {
    let collector = Collector { schema, concrete };
    value
        .alternatives
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            let mut tree = SelectionTree::default();
            collector
                .entry(entry, true, &mut tree)
                .then_some((index, tree))
        })
        .collect()
}

/// `IsArgumentMappable` (spec §4 "Lookup Key Missing For Type"): some top-level alternative of
/// `value` admits `concrete` and every field it references at the root is defined on `concrete`.
pub(crate) fn is_mappable(value: &SelectedValue, schema: &Schema, concrete: &Name) -> bool {
    let Some(apollo_compiler::schema::ExtendedType::Object(object)) = schema.types.get(concrete)
    else {
        return false;
    };
    selections_for_type(value, schema, concrete)
        .iter()
        .any(|(_, tree)| {
            tree.top_level_fields()
                .all(|f| object.fields.contains_key(f))
        })
}
