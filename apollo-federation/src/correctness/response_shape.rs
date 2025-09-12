use std::fmt;
use std::sync::Arc;

use apollo_compiler::ExecutableDocument;
use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::executable::Field;
use apollo_compiler::executable::Fragment;
use apollo_compiler::executable::FragmentMap;
use apollo_compiler::executable::Operation;
use apollo_compiler::executable::Selection;
use apollo_compiler::executable::SelectionSet;
use apollo_compiler::validation::Valid;

use crate::FederationError;
use crate::bail;
use crate::display_helpers;
use crate::ensure;
use crate::internal_error;
use crate::schema::ValidFederationSchema;
use crate::schema::position::CompositeTypeDefinitionPosition;
use crate::schema::position::INTROSPECTION_TYPENAME_FIELD_NAME;
use crate::schema::position::InterfaceTypeDefinitionPosition;
use crate::schema::position::ObjectTypeDefinitionPosition;
use crate::utils::FallibleIterator;

//==================================================================================================
// Vec utilities

fn vec_sorted_by<T: Clone>(src: &[T], compare: impl Fn(&T, &T) -> std::cmp::Ordering) -> Vec<T> {
    let mut sorted = src.to_owned();
    sorted.sort_by(&compare);
    sorted
}

//==================================================================================================
// Type conditions

fn get_interface_implementers<'a>(
    interface: &InterfaceTypeDefinitionPosition,
    schema: &'a ValidFederationSchema,
) -> Result<&'a IndexSet<ObjectTypeDefinitionPosition>, FederationError> {
    Ok(&schema
        .referencers()
        .get_interface_type(&interface.type_name)?
        .object_types)
}

/// Does `x` implies `y`? (`x`'s possible types is a subset of `y`'s possible types)
/// - All type-definition positions are in the given schema.
// Note: Similar to `runtime_types_intersect` (avoids using `possible_runtime_types`)
fn runtime_types_implies(
    x: &CompositeTypeDefinitionPosition,
    y: &CompositeTypeDefinitionPosition,
    schema: &ValidFederationSchema,
) -> Result<bool, FederationError> {
    use CompositeTypeDefinitionPosition::*;
    match (x, y) {
        (Object(x), Object(y)) => Ok(x == y),
        (Object(object), Union(union)) => {
            // Union members must be object types in GraphQL.
            let union_type = union.get(schema.schema())?;
            Ok(union_type.members.contains(&object.type_name))
        }
        (Union(union), Object(object)) => {
            // Is `object` the only member of `union`?
            let union_type = union.get(schema.schema())?;
            Ok(union_type.members.len() == 1 && union_type.members.contains(&object.type_name))
        }
        (Object(object), Interface(interface)) => {
            // Interface implementers must be object types in GraphQL.
            let interface_implementers = get_interface_implementers(interface, schema)?;
            Ok(interface_implementers.contains(object))
        }
        (Interface(interface), Object(object)) => {
            // Is `object` the only implementer of `interface`?
            let interface_implementers = get_interface_implementers(interface, schema)?;
            Ok(interface_implementers.len() == 1 && interface_implementers.contains(object))
        }

        (Union(x), Union(y)) if x == y => Ok(true),
        (Union(x), Union(y)) => {
            let (x, y) = (x.get(schema.schema())?, y.get(schema.schema())?);
            Ok(x.members.is_subset(&y.members))
        }

        (Interface(x), Interface(y)) if x == y => Ok(true),
        (Interface(x), Interface(y)) => {
            let x = get_interface_implementers(x, schema)?;
            let y = get_interface_implementers(y, schema)?;
            Ok(x.is_subset(y))
        }

        (Union(union), Interface(interface)) => {
            let union = union.get(schema.schema())?;
            let interface_implementers = get_interface_implementers(interface, schema)?;
            Ok(union.members.iter().all(|m| {
                let m_ty = ObjectTypeDefinitionPosition::new(m.name.clone());
                interface_implementers.contains(&m_ty)
            }))
        }
        (Interface(interface), Union(union)) => {
            let interface_implementers = get_interface_implementers(interface, schema)?;
            let union = union.get(schema.schema())?;
            Ok(interface_implementers
                .iter()
                .all(|t| union.members.contains(&t.type_name)))
        }
    }
}

/// Constructs a set of object types
/// - Slow: calls `possible_runtime_types` and sorts the result.
/// - Note: May return an empty set if the type has no runtime types.
fn get_ground_types(
    ty: &CompositeTypeDefinitionPosition,
    schema: &ValidFederationSchema,
) -> Result<Vec<ObjectTypeDefinitionPosition>, FederationError> {
    let mut result = schema.possible_runtime_types(ty.clone())?;
    result.sort_by(|a, b| a.type_name.cmp(&b.type_name));
    Ok(result.into_iter().collect())
}

/// A sequence of type conditions applied (used for display)
// - This displays a type condition as an intersection of named types.
// - If the vector is empty, it means a "deduced type condition".
//   Thus, we may not know how to display such a composition of types.
//   That can happen when a more specific type condition is computed
//   than the one that was explicitly provided.
#[derive(Debug, Clone)]
struct DisplayTypeCondition(Vec<CompositeTypeDefinitionPosition>);

impl DisplayTypeCondition {
    fn new(ty: CompositeTypeDefinitionPosition) -> Self {
        DisplayTypeCondition(vec![ty])
    }

    fn deduced() -> Self {
        DisplayTypeCondition(Vec::new())
    }

    /// Construct a new type condition with a named type condition added.
    fn add_type_name(
        &self,
        name: Name,
        schema: &ValidFederationSchema,
    ) -> Result<Self, FederationError> {
        let ty: CompositeTypeDefinitionPosition = schema.get_type(name)?.try_into()?;
        if self
            .0
            .iter()
            .fallible_any(|t| runtime_types_implies(t, &ty, schema))?
        {
            return Ok(self.clone());
        }
        // filter out existing conditions that are implied by `ty`.
        let mut buf = Vec::new();
        for t in &self.0 {
            if !runtime_types_implies(&ty, t, schema)? {
                buf.push(t.clone());
            }
        }
        buf.push(ty);
        buf.sort_by(|a, b| a.type_name().cmp(b.type_name()));
        Ok(DisplayTypeCondition(buf))
    }
}

/// Aggregated type conditions that are normalized for comparison
#[derive(Debug, Clone)]
pub struct NormalizedTypeCondition {
    // The set of object types that are used for comparison.
    // - The ground_set must be non-empty.
    // - The ground_set must be sorted by type name.
    ground_set: Vec<ObjectTypeDefinitionPosition>,

    // Simplified type condition for display.
    for_display: DisplayTypeCondition,
}

impl PartialEq for NormalizedTypeCondition {
    fn eq(&self, other: &Self) -> bool {
        self.ground_set == other.ground_set
    }
}

impl Eq for NormalizedTypeCondition {}

impl std::hash::Hash for NormalizedTypeCondition {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.ground_set.hash(state);
    }
}

// Public constructors & accessors
impl NormalizedTypeCondition {
    /// Construct a new type condition with a single named type condition.
    /// - Returns None if the name type has no runtime types (an interface with no implementors).
    pub(crate) fn from_type_name(
        name: Name,
        schema: &ValidFederationSchema,
    ) -> Result<Option<Self>, FederationError> {
        let ty: CompositeTypeDefinitionPosition = schema.get_type(name)?.try_into()?;
        let ground_set = get_ground_types(&ty, schema)?;
        if ground_set.is_empty() {
            return Ok(None);
        }
        Ok(Some(NormalizedTypeCondition {
            ground_set,
            for_display: DisplayTypeCondition::new(ty),
        }))
    }

    pub(crate) fn from_object_type(ty: &ObjectTypeDefinitionPosition) -> Self {
        NormalizedTypeCondition {
            ground_set: vec![ty.clone()],
            for_display: DisplayTypeCondition::new(ty.clone().into()),
        }
    }

    /// Precondition: `types` must be non-empty.
    pub(crate) fn from_object_types(
        types: impl Iterator<Item = ObjectTypeDefinitionPosition>,
    ) -> Result<Self, FederationError> {
        let mut ground_set: Vec<_> = types.collect();
        if ground_set.is_empty() {
            bail!("Unexpected empty type list for from_object_types")
        }
        ground_set.sort_by(|a, b| a.type_name.cmp(&b.type_name));
        Ok(NormalizedTypeCondition {
            ground_set,
            for_display: DisplayTypeCondition::deduced(),
        })
    }

    pub(crate) fn ground_set(&self) -> &[ObjectTypeDefinitionPosition] {
        &self.ground_set
    }

    /// Is this type condition represented by a single named type?
    pub fn is_named_type(&self, type_name: &Name) -> bool {
        // Check the display type first.
        let Some((first, rest)) = self.for_display.0.split_first() else {
            return false;
        };
        if rest.is_empty() && first.type_name() == type_name {
            return true;
        }

        // Check the ground set.
        let Some((first, rest)) = self.ground_set.split_first() else {
            return false;
        };
        rest.is_empty() && first.type_name == *type_name
    }

    /// Is this type condition a named object type?
    pub fn is_named_object_type(&self) -> bool {
        let Some((display_first, display_rest)) = self.for_display.0.split_first() else {
            // Deduced condition is not an object type.
            return false;
        };
        display_rest.is_empty() && display_first.is_object_type()
    }

    pub fn implies(&self, other: &Self) -> bool {
        self.ground_set.iter().all(|t| other.ground_set.contains(t))
    }
}

impl NormalizedTypeCondition {
    /// Construct a new type condition with a named type condition added.
    /// - Returns None if the new type condition is unsatisfiable.
    pub(crate) fn add_type_name(
        &self,
        name: Name,
        schema: &ValidFederationSchema,
    ) -> Result<Option<Self>, FederationError> {
        let other_ty: CompositeTypeDefinitionPosition =
            schema.get_type(name.clone())?.try_into()?;
        let other_types = get_ground_types(&other_ty, schema)?;
        let ground_set: Vec<ObjectTypeDefinitionPosition> = self
            .ground_set
            .iter()
            .filter(|t| other_types.contains(t))
            .cloned()
            .collect();
        if ground_set.is_empty() {
            // Unsatisfiable condition
            Ok(None)
        } else {
            let for_display = if ground_set.len() == self.ground_set.len() {
                // unchanged
                self.for_display.clone()
            } else {
                self.for_display.add_type_name(name, schema)?
            };
            Ok(Some(NormalizedTypeCondition {
                ground_set,
                for_display,
            }))
        }
    }

    /// Compute the `field`'s type condition considering the parent type condition.
    /// - Returns None if the resulting type condition has no possible object types.
    fn field_type_condition(
        &self,
        field: &Field,
        schema: &ValidFederationSchema,
    ) -> Result<Option<Self>, FederationError> {
        let declared_type = field.ty().inner_named_type();

        // Collect all possible object types for the field in the given parent type condition.
        let mut types = IndexSet::default();
        for ty_pos in &self.ground_set {
            let ty_def = ty_pos.get(schema.schema())?;
            let Some(field_def) = ty_def.fields.get(&field.name) else {
                continue;
            };
            let field_ty = field_def.ty.inner_named_type().clone();
            types.insert(field_ty);
        }

        // Simple case #1 - The collected types is just a single named type.
        if types.len() == 1
            && let Some(first) = types.first()
        {
            return NormalizedTypeCondition::from_type_name(first.clone(), schema);
        }

        // Grind the type names into object types.
        let mut ground_types = IndexSet::default();
        for ty in &types {
            let pos = schema.get_type(ty.clone())?.try_into()?;
            let pos_types = schema.possible_runtime_types(pos)?;
            ground_types.extend(pos_types.into_iter());
        }
        if ground_types.is_empty() {
            return Ok(None);
        }

        // Simple case #2 - `declared_type` is same as the collected types.
        if let Some(declared_type_cond) =
            NormalizedTypeCondition::from_type_name(declared_type.clone(), schema)?
            && declared_type_cond.ground_set.len() == ground_types.len()
            && declared_type_cond
                .ground_set
                .iter()
                .all(|t| ground_types.contains(t))
        {
            return Ok(Some(declared_type_cond));
        }

        Ok(Some(NormalizedTypeCondition::from_object_types(
            ground_types.into_iter(),
        )?))
    }
}

//==================================================================================================
// Boolean conditions

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Literal {
    Pos(Name), // positive occurrence of the variable with the given name
    Neg(Name), // negated variable with the given name
}

impl Literal {
    pub fn variable(&self) -> &Name {
        match self {
            Literal::Pos(name) | Literal::Neg(name) => name,
        }
    }

    pub fn polarity(&self) -> bool {
        matches!(self, Literal::Pos(_))
    }
}

// A clause is a conjunction of literals.
// Empty Clause means "true".
// "false" can't be represented. Any cases with false condition must be dropped entirely.
// This vector must be sorted by the variable name.
// This vector must be deduplicated (every variant appears only once).
// Thus, no conflicting literals are allowed (e.g., `x` and `¬x`).
#[derive(Debug, Clone, Default, Eq)]
pub struct Clause(Vec<Literal>);

impl Clause {
    pub fn literals(&self) -> &[Literal] {
        &self.0
    }

    pub fn is_always_true(&self) -> bool {
        self.0.is_empty()
    }

    /// check if `self` implies `other`
    /// - The literals in `other` is a subset of `self`.
    pub fn implies(&self, other: &Clause) -> bool {
        let mut self_variables: IndexMap<Name, bool> = IndexMap::default();
        // Assume that `self` has no conflicts.
        for lit in &self.0 {
            self_variables.insert(lit.variable().clone(), lit.polarity());
        }
        other.0.iter().all(|lit| {
            self_variables
                .get(lit.variable())
                .is_some_and(|pol| *pol == lit.polarity())
        })
    }

    /// Creates a clause from a vector of literals.
    pub fn from_literals(literals: &[Literal]) -> Self {
        let variables: IndexMap<Name, bool> = literals
            .iter()
            .map(|lit| (lit.variable().clone(), lit.polarity()))
            .collect();
        Self::from_variable_map(&variables)
    }

    /// Creates a clause from a variable-to-Boolean mapping.
    /// variables: variable name (Name) -> polarity (bool)
    fn from_variable_map(variables: &IndexMap<Name, bool>) -> Self {
        let mut buf: Vec<Literal> = variables
            .iter()
            .map(|(name, polarity)| match polarity {
                false => Literal::Neg(name.clone()),
                true => Literal::Pos(name.clone()),
            })
            .collect();
        buf.sort_by(|a, b| a.variable().cmp(b.variable()));
        Clause(buf)
    }

    /// `self` ∧ `other` (logical conjunction of clauses, which is also set-union)
    /// - Returns None if there is a conflict.
    pub fn concatenate(&self, other: &Clause) -> Option<Clause> {
        let mut variables: IndexMap<Name, bool> = IndexMap::default();
        // Assume that `self` has no conflicts.
        for lit in &self.0 {
            variables.insert(lit.variable().clone(), lit.polarity());
        }
        for lit in &other.0 {
            let var = lit.variable();
            let entry = variables.entry(var.clone()).or_insert(lit.polarity());
            if *entry != lit.polarity() {
                return None; // conflict
            }
        }
        Some(Self::from_variable_map(&variables))
    }

    /// `self` - `other` (set subtraction)
    /// - Returns None if `self` and `other` are conflicting.
    pub fn subtract(&self, other: &Clause) -> Option<Clause> {
        let mut other_variables: IndexMap<Name, bool> = IndexMap::default();
        for lit in &other.0 {
            other_variables.insert(lit.variable().clone(), lit.polarity());
        }

        let mut variables: IndexMap<Name, bool> = IndexMap::default();
        for lit in &self.0 {
            let var = lit.variable();
            if let Some(pol) = other_variables.get(var) {
                if *pol == lit.polarity() {
                    // Match => Skip `lit`
                    continue;
                } else {
                    // Conflict
                    return None;
                }
            } else {
                // Keep `lit`
                variables.insert(var.clone(), lit.polarity());
            }
        }
        Some(Self::from_variable_map(&variables))
    }

    fn add_selection_directives(
        &self,
        directives: &ast::DirectiveList,
    ) -> Result<Option<Clause>, FederationError> {
        let Some(selection_clause) = boolean_clause_from_directives(directives)? else {
            // The condition is unsatisfiable within the field itself.
            return Ok(None);
        };
        Ok(self.concatenate(&selection_clause))
    }

    /// Returns a clause with everything included and a simplified version of the `clause`.
    /// - The simplified clause does not include variables that are already in `self`.
    pub fn concatenate_and_simplify(&self, clause: &Clause) -> Option<(Clause, Clause)> {
        let mut all_variables: IndexMap<Name, bool> = IndexMap::default();
        // Load `self` on `variables`.
        // - Assume that `self` has no conflicts.
        for lit in &self.0 {
            all_variables.insert(lit.variable().clone(), lit.polarity());
        }

        let mut added_variables: IndexMap<Name, bool> = IndexMap::default();
        for lit in &clause.0 {
            let var = lit.variable();
            match all_variables.entry(var.clone()) {
                indexmap::map::Entry::Occupied(entry) => {
                    if entry.get() != &lit.polarity() {
                        return None; // conflict
                    }
                }
                indexmap::map::Entry::Vacant(entry) => {
                    entry.insert(lit.polarity());
                    added_variables.insert(var.clone(), lit.polarity());
                }
            }
        }
        Some((
            Self::from_variable_map(&all_variables),
            Self::from_variable_map(&added_variables),
        ))
    }
}

impl PartialEq for Clause {
    fn eq(&self, other: &Self) -> bool {
        // assume: The underlying vectors are deduplicated.
        self.0.len() == other.0.len() && self.0.iter().all(|l| other.0.contains(l))
    }
}

//==================================================================================================
// Normalization of Field Selection

/// Extracts the Boolean clause from the directive list.
// Similar to `Conditions::from_directives` in `conditions.rs`.
fn boolean_clause_from_directives(
    directives: &ast::DirectiveList,
) -> Result<Option<Clause>, FederationError> {
    let mut variables = IndexMap::default(); // variable name (Name) -> polarity (bool)
    if let Some(skip) = directives.get("skip") {
        let Some(value) = skip.specified_argument_by_name("if") else {
            bail!("missing @skip(if:) argument")
        };

        match value.as_ref() {
            // Constant @skip(if: true) can never match
            ast::Value::Boolean(true) => return Ok(None),
            // Constant @skip(if: false) always matches
            ast::Value::Boolean(_) => {}
            ast::Value::Variable(name) => {
                variables.insert(name.clone(), false);
            }
            _ => {
                bail!("expected boolean or variable `if` argument, got {value}")
            }
        }
    }

    if let Some(include) = directives.get("include") {
        let Some(value) = include.specified_argument_by_name("if") else {
            bail!("missing @include(if:) argument")
        };

        match value.as_ref() {
            // Constant @include(if: false) can never match
            ast::Value::Boolean(false) => return Ok(None),
            // Constant @include(if: true) always matches
            ast::Value::Boolean(true) => {}
            // If both @skip(if: $var) and @include(if: $var) exist, the condition can also
            // never match
            ast::Value::Variable(name) => {
                if variables.insert(name.clone(), true) == Some(false) {
                    // Conflict found
                    return Ok(None);
                }
            }
            _ => {
                bail!("expected boolean or variable `if` argument, got {value}")
            }
        }
    }
    Ok(Some(Clause::from_variable_map(&variables)))
}

fn normalize_ast_value(v: &mut ast::Value) {
    // special cases
    match v {
        // Sort object fields by name
        ast::Value::Object(fields) => {
            fields.sort_by(|a, b| a.0.cmp(&b.0));
            for (_name, value) in fields {
                normalize_ast_value(value.make_mut());
            }
        }

        // Recurse into list items.
        ast::Value::List(items) => {
            for value in items {
                normalize_ast_value(value.make_mut());
            }
        }

        _ => (), // otherwise, do nothing
    }
}

fn normalized_arguments(args: &[Node<ast::Argument>]) -> Vec<Node<ast::Argument>> {
    // sort by name
    let mut args = vec_sorted_by(args, |a, b| a.name.cmp(&b.name));
    // normalize argument values in place
    for arg in &mut args {
        normalize_ast_value(arg.make_mut().value.make_mut());
    }
    args
}

fn remove_conditions_from_directives(directives: &ast::DirectiveList) -> ast::DirectiveList {
    directives
        .iter()
        .filter(|d| d.name != "skip" && d.name != "include")
        .cloned()
        .collect()
}

pub type FieldSelectionKey = Field;

// Extract the selection key
fn field_selection_key(field: &Field) -> FieldSelectionKey {
    Field {
        definition: field.definition.clone(),
        alias: None, // not used for comparison
        name: field.name.clone(),
        arguments: normalized_arguments(&field.arguments),
        directives: ast::DirectiveList::default(), // not used for comparison
        selection_set: SelectionSet::new(field.selection_set.ty.clone()), // not used for comparison
    }
}

fn eq_field_selection_key(a: &FieldSelectionKey, b: &FieldSelectionKey) -> bool {
    // Note: Arguments are expected to be normalized.
    a.name == b.name && a.arguments == b.arguments
}

//==================================================================================================
// ResponseShape

/// Simplified field value used for display purposes
fn field_display(field: &Field) -> Field {
    Field {
        definition: field.definition.clone(),
        alias: None, // not used for display
        name: field.name.clone(),
        arguments: field.arguments.clone(),
        directives: remove_conditions_from_directives(&field.directives),
        selection_set: SelectionSet::new(field.selection_set.ty.clone()), // not used for display
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct DefinitionVariant {
    /// Boolean clause is the secondary key after NormalizedTypeCondition as primary key.
    boolean_clause: Clause,

    /// Representative field selection for definition/display (see `fn field_display`).
    /// - This is the first field of the same field selection key in depth-first order as
    ///   defined by `CollectFields` and `ExecuteField` algorithms in the GraphQL spec.
    representative_field: Field,

    /// Different variants can have different sets of sub-selections (if any).
    sub_selection_response_shape: Option<ResponseShape>,
}

impl DefinitionVariant {
    pub fn boolean_clause(&self) -> &Clause {
        &self.boolean_clause
    }

    pub fn representative_field(&self) -> &Field {
        &self.representative_field
    }

    pub fn sub_selection_response_shape(&self) -> Option<&ResponseShape> {
        self.sub_selection_response_shape.as_ref()
    }

    pub fn with_updated_clause(&self, boolean_clause: Clause) -> Self {
        DefinitionVariant {
            boolean_clause,
            representative_field: self.representative_field.clone(),
            sub_selection_response_shape: self.sub_selection_response_shape.clone(),
        }
    }

    pub fn with_updated_sub_selection_response_shape(&self, new_shape: ResponseShape) -> Self {
        DefinitionVariant {
            boolean_clause: self.boolean_clause.clone(),
            representative_field: self.representative_field.clone(),
            sub_selection_response_shape: Some(new_shape),
        }
    }

    pub fn with_updated_fields(
        &self,
        boolean_clause: Clause,
        sub_selection_response_shape: Option<ResponseShape>,
    ) -> Self {
        DefinitionVariant {
            boolean_clause,
            sub_selection_response_shape,
            representative_field: self.representative_field.clone(),
        }
    }

    pub fn new(
        boolean_clause: Clause,
        representative_field: Field,
        sub_selection_response_shape: Option<ResponseShape>,
    ) -> Self {
        DefinitionVariant {
            boolean_clause,
            representative_field,
            sub_selection_response_shape,
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct PossibleDefinitionsPerTypeCondition {
    /// The key for comparison (only used for GraphQL invariant check).
    /// - Under each type condition, all variants must have the same selection key.
    field_selection_key: FieldSelectionKey,

    /// Under each type condition, there may be multiple variants with different Boolean conditions.
    conditional_variants: Vec<DefinitionVariant>,
    // - Every variant's Boolean condition must be unique.
    // - Note: The Boolean conditions between variants may not be mutually exclusive.
}

impl PossibleDefinitionsPerTypeCondition {
    pub fn field_selection_key(&self) -> &FieldSelectionKey {
        &self.field_selection_key
    }

    pub fn conditional_variants(&self) -> &[DefinitionVariant] {
        &self.conditional_variants
    }

    pub fn with_updated_conditional_variants(&self, new_variants: Vec<DefinitionVariant>) -> Self {
        PossibleDefinitionsPerTypeCondition {
            field_selection_key: self.field_selection_key.clone(),
            conditional_variants: new_variants,
        }
    }

    pub fn new(
        field_selection_key: FieldSelectionKey,
        conditional_variants: Vec<DefinitionVariant>,
    ) -> Self {
        PossibleDefinitionsPerTypeCondition {
            field_selection_key,
            conditional_variants,
        }
    }

    pub(crate) fn insert_variant(
        &mut self,
        variant: DefinitionVariant,
    ) -> Result<(), FederationError> {
        for existing in &mut self.conditional_variants {
            if existing.boolean_clause == variant.boolean_clause {
                // Merge response shapes (MergeSelectionSets from GraphQL spec 6.4.3)
                match (
                    &mut existing.sub_selection_response_shape,
                    variant.sub_selection_response_shape,
                ) {
                    (None, None) => {} // nothing to do
                    (Some(existing_rs), Some(ref variant_rs)) => {
                        existing_rs.merge_with(variant_rs)?;
                    }
                    (None, Some(_)) | (Some(_), None) => {
                        unreachable!("mismatched sub-selection options")
                    }
                }
                return Ok(());
            }
        }
        self.conditional_variants.push(variant);
        Ok(())
    }
}

/// All possible definitions that a response key can have.
/// - At the top level, all possibilities are indexed by the type condition.
/// - However, they are not necessarily mutually exclusive.
#[derive(Debug, Default, PartialEq, Eq, Clone)]
pub struct PossibleDefinitions(
    IndexMap<NormalizedTypeCondition, PossibleDefinitionsPerTypeCondition>,
);

// Public accessors
impl PossibleDefinitions {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn iter(
        &self,
    ) -> impl Iterator<
        Item = (
            &NormalizedTypeCondition,
            &PossibleDefinitionsPerTypeCondition,
        ),
    > {
        self.0.iter()
    }

    pub fn get(
        &self,
        type_cond: &NormalizedTypeCondition,
    ) -> Option<&PossibleDefinitionsPerTypeCondition> {
        self.0.get(type_cond)
    }

    pub fn insert(
        &mut self,
        type_condition: NormalizedTypeCondition,
        value: PossibleDefinitionsPerTypeCondition,
    ) -> bool {
        self.0.insert(type_condition, value).is_some()
    }
}

impl PossibleDefinitions {
    fn insert_possible_definition(
        &mut self,
        type_conditions: NormalizedTypeCondition,
        boolean_clause: Clause, // the aggregate boolean condition of the current selection set
        representative_field: Field,
        sub_selection_response_shape: Option<ResponseShape>,
    ) -> Result<(), FederationError> {
        let field_selection_key = field_selection_key(&representative_field);
        let entry = self.0.entry(type_conditions);
        let insert_variant = |per_type_cond: &mut PossibleDefinitionsPerTypeCondition| {
            let value = DefinitionVariant {
                boolean_clause,
                representative_field,
                sub_selection_response_shape,
            };
            per_type_cond.insert_variant(value)
        };
        match entry {
            indexmap::map::Entry::Vacant(e) => {
                // New type condition
                let empty_per_type_cond = PossibleDefinitionsPerTypeCondition {
                    field_selection_key,
                    conditional_variants: vec![],
                };
                insert_variant(e.insert(empty_per_type_cond))?;
            }
            indexmap::map::Entry::Occupied(mut e) => {
                // GraphQL invariant: per_type_cond.field_selection_key must be the same
                //                    as the given field_selection_key.
                if !eq_field_selection_key(&e.get().field_selection_key, &field_selection_key) {
                    return Err(internal_error!(
                        "field_selection_key was expected to be the same\nexisting: {}\nadding: {}",
                        e.get().field_selection_key,
                        field_selection_key,
                    ));
                }
                insert_variant(e.get_mut())?;
            }
        };
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct ResponseShape {
    /// The default type condition is only used for display.
    default_type_condition: Name,
    definitions_per_response_key: IndexMap</*response_key*/ Name, PossibleDefinitions>,
}

impl ResponseShape {
    pub fn default_type_condition(&self) -> &Name {
        &self.default_type_condition
    }

    pub fn is_empty(&self) -> bool {
        self.definitions_per_response_key.is_empty()
    }

    pub fn len(&self) -> usize {
        self.definitions_per_response_key.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Name, &PossibleDefinitions)> {
        self.definitions_per_response_key.iter()
    }

    pub fn get(&self, response_key: &Name) -> Option<&PossibleDefinitions> {
        self.definitions_per_response_key.get(response_key)
    }

    pub fn insert(&mut self, response_key: Name, value: PossibleDefinitions) -> bool {
        self.definitions_per_response_key
            .insert(response_key, value)
            .is_some()
    }

    pub fn new(default_type_condition: Name) -> Self {
        ResponseShape {
            default_type_condition,
            definitions_per_response_key: IndexMap::default(),
        }
    }

    pub fn merge_with(&mut self, other: &Self) -> Result<(), FederationError> {
        for (response_key, other_defs) in &other.definitions_per_response_key {
            let value = self
                .definitions_per_response_key
                .entry(response_key.clone())
                .or_default();
            for (type_condition, per_type_cond) in &other_defs.0 {
                for variant in &per_type_cond.conditional_variants {
                    value.insert_possible_definition(
                        type_condition.clone(),
                        variant.boolean_clause.clone(),
                        variant.representative_field.clone(),
                        variant.sub_selection_response_shape.clone(),
                    )?;
                }
            }
        }
        Ok(())
    }
}

//==================================================================================================
// ResponseShape computation from operation

struct ResponseShapeContext {
    schema: ValidFederationSchema,
    fragment_defs: Arc<IndexMap<Name, Node<Fragment>>>, // fragment definitions in the operation
    parent_type: Name,                                  // the type of the current selection set
    type_condition: NormalizedTypeCondition, // accumulated type condition down from the parent field.
    inherited_clause: Clause, // accumulated conditions from the root up to parent field
    current_clause: Clause,   // accumulated conditions down from the parent field
    skip_introspection: bool, // true for input operation's root contexts only
}

impl ResponseShapeContext {
    fn process_selection(
        &self,
        response_shape: &mut ResponseShape,
        selection: &Selection,
    ) -> Result<(), FederationError> {
        match selection {
            Selection::Field(field) => self.process_field_selection(response_shape, field),
            Selection::FragmentSpread(fragment_spread) => {
                let fragment_def =
                    get_fragment_definition(&self.fragment_defs, &fragment_spread.fragment_name)?;
                // Note: `@skip/@include` directives are not allowed on fragment definitions.
                //       Thus, no need to check their directives for Boolean conditions.
                self.process_fragment_selection(
                    response_shape,
                    fragment_def.type_condition(),
                    &fragment_spread.directives,
                    &fragment_def.selection_set,
                )
            }
            Selection::InlineFragment(inline_fragment) => {
                let fragment_type_condition = inline_fragment
                    .type_condition
                    .as_ref()
                    .unwrap_or(&self.parent_type);
                self.process_fragment_selection(
                    response_shape,
                    fragment_type_condition,
                    &inline_fragment.directives,
                    &inline_fragment.selection_set,
                )
            }
        }
    }

    fn process_field_selection(
        &self,
        response_shape: &mut ResponseShape,
        field: &Node<Field>,
    ) -> Result<(), FederationError> {
        // Skip __typename fields in the input root context.
        if self.skip_introspection && field.name == *INTROSPECTION_TYPENAME_FIELD_NAME {
            return Ok(());
        }
        // Skip introspection fields since QP ignores them.
        // (see comments on `FieldSelection::from_field`)
        if is_introspection_field_name(&field.name) {
            return Ok(());
        }
        let Some(field_clause) = self
            .current_clause
            .add_selection_directives(&field.directives)?
        else {
            // Unsatisfiable local condition under the parent field => skip
            return Ok(());
        };
        let Some((inherited_clause, field_clause)) = self
            .inherited_clause
            .concatenate_and_simplify(&field_clause)
        else {
            // Unsatisfiable full condition from the root => skip
            return Ok(());
        };
        // Process the field's sub-selection
        let sub_selection_response_shape: Option<ResponseShape> = if field.selection_set.is_empty()
        {
            None
        } else {
            // The field's declared type may not be the most specific type (in case of up-casting).

            // internal invariant check
            ensure!(
                *field.ty().inner_named_type() == field.selection_set.ty,
                "internal invariant failure: field's type does not match with its selection set's type"
            );

            // A brand new context with the new type condition.
            // - Still inherits the boolean conditions for simplification purposes.
            let parent_type = field.selection_set.ty.clone();
            self.type_condition
                .field_type_condition(field, &self.schema)?
                .map(|type_condition| {
                    let context = ResponseShapeContext {
                        schema: self.schema.clone(),
                        fragment_defs: self.fragment_defs.clone(),
                        parent_type,
                        type_condition,
                        inherited_clause,
                        current_clause: Clause::default(), // empty
                        skip_introspection: false,         // false by default
                    };
                    context.process_selection_set(&field.selection_set)
                })
                .transpose()?
        };
        // Record this selection's definition.
        let value = response_shape
            .definitions_per_response_key
            .entry(field.response_key().clone())
            .or_default();
        value.insert_possible_definition(
            self.type_condition.clone(),
            field_clause,
            field_display(field),
            sub_selection_response_shape,
        )
    }

    /// For both inline fragments and fragment spreads
    fn process_fragment_selection(
        &self,
        response_shape: &mut ResponseShape,
        fragment_type_condition: &Name,
        directives: &ast::DirectiveList,
        selection_set: &SelectionSet,
    ) -> Result<(), FederationError> {
        // internal invariant check
        ensure!(
            *fragment_type_condition == selection_set.ty,
            "internal invariant failure: fragment's type condition does not match with its selection set's type"
        );

        let Some(type_condition) = NormalizedTypeCondition::add_type_name(
            &self.type_condition,
            fragment_type_condition.clone(),
            &self.schema,
        )?
        else {
            // Unsatisfiable type condition => skip
            return Ok(());
        };
        let Some(current_clause) = self.current_clause.add_selection_directives(directives)? else {
            // Unsatisfiable local condition under the parent field => skip
            return Ok(());
        };
        // check if `self.inherited_clause` and `current_clause` are unsatisfiable together.
        if self.inherited_clause.concatenate(&current_clause).is_none() {
            // Unsatisfiable full condition from the root => skip
            return Ok(());
        }

        // The inner context with a new type condition.
        // Note: Non-conditional directives on inline spreads are ignored.
        let context = ResponseShapeContext {
            schema: self.schema.clone(),
            fragment_defs: self.fragment_defs.clone(),
            parent_type: fragment_type_condition.clone(),
            type_condition,
            inherited_clause: self.inherited_clause.clone(), // no change
            current_clause,
            skip_introspection: self.skip_introspection,
        };
        context.process_selection_set_within(response_shape, selection_set)
    }

    /// Using an existing response shape
    fn process_selection_set_within(
        &self,
        response_shape: &mut ResponseShape,
        selection_set: &SelectionSet,
    ) -> Result<(), FederationError> {
        for selection in &selection_set.selections {
            self.process_selection(response_shape, selection)?;
        }
        Ok(())
    }

    /// For a new sub-ResponseShape
    /// - This corresponds to the `CollectFields` algorithm in the GraphQL specification.
    fn process_selection_set(
        &self,
        selection_set: &SelectionSet,
    ) -> Result<ResponseShape, FederationError> {
        let mut response_shape = ResponseShape::new(selection_set.ty.clone());
        self.process_selection_set_within(&mut response_shape, selection_set)?;
        Ok(response_shape)
    }
}

fn is_introspection_field_name(name: &Name) -> bool {
    name == "__schema" || name == "__type"
}

fn get_operation_and_fragment_definitions(
    operation_doc: &Valid<ExecutableDocument>,
) -> Result<(Node<Operation>, Arc<FragmentMap>), FederationError> {
    let mut op_iter = operation_doc.operations.iter();
    let Some(first) = op_iter.next() else {
        bail!("Operation not found")
    };
    if op_iter.next().is_some() {
        bail!("Multiple operations are not supported")
    }

    let fragment_defs = Arc::new(operation_doc.fragments.clone());
    Ok((first.clone(), fragment_defs))
}

fn get_fragment_definition<'a>(
    fragment_defs: &'a Arc<IndexMap<Name, Node<Fragment>>>,
    fragment_name: &Name,
) -> Result<&'a Node<Fragment>, FederationError> {
    let fragment_def = fragment_defs
        .get(fragment_name)
        .ok_or_else(|| internal_error!("Fragment definition not found: {}", fragment_name))?;
    Ok(fragment_def)
}

pub fn compute_response_shape_for_operation(
    operation_doc: &Valid<ExecutableDocument>,
    schema: &ValidFederationSchema,
) -> Result<ResponseShape, FederationError> {
    let (operation, fragment_defs) = get_operation_and_fragment_definitions(operation_doc)?;

    // Start a new root context and process the root selection set.
    // - Not using `process_selection_set` because there is no parent context.
    let parent_type = operation.selection_set.ty.clone();
    let Some(type_condition) =
        NormalizedTypeCondition::from_type_name(parent_type.clone(), schema)?
    else {
        bail!("Unexpected empty type condition for the root type: {parent_type}")
    };
    let context = ResponseShapeContext {
        schema: schema.clone(),
        fragment_defs,
        parent_type,
        type_condition,
        inherited_clause: Clause::default(), // empty
        current_clause: Clause::default(),   // empty
        skip_introspection: true,            // true for root context
    };
    context.process_selection_set(&operation.selection_set)
}

pub fn compute_the_root_type_condition_for_operation(
    operation_doc: &Valid<ExecutableDocument>,
) -> Result<Name, FederationError> {
    let (operation, _) = get_operation_and_fragment_definitions(operation_doc)?;
    Ok(operation.selection_set.ty.clone())
}

/// Entity fetch operation may have multiple entity selections.
/// This function returns a vector of response shapes per each individual entity selection.
pub fn compute_response_shape_for_entity_fetch_operation(
    operation_doc: &Valid<ExecutableDocument>,
    schema: &ValidFederationSchema,
) -> Result<Vec<ResponseShape>, FederationError> {
    let (operation, fragment_defs) = get_operation_and_fragment_definitions(operation_doc)?;

    // drill down the `_entities` selection set
    let mut sel_iter = operation.selection_set.selections.iter();
    let Some(first_selection) = sel_iter.next() else {
        bail!("Entity fetch is expected to have at least one selection")
    };
    if sel_iter.next().is_some() {
        bail!("Entity fetch is expected to have exactly one selection")
    }
    let Selection::Field(field) = first_selection else {
        bail!("Entity fetch is expected to have a field selection only")
    };
    if field.name != crate::subgraph::spec::ENTITIES_QUERY {
        bail!("Entity fetch is expected to have a field selection named `_entities`")
    }

    field
        .selection_set
        .selections
        .iter()
        .map(|selection| {
            let type_condition = get_fragment_type_condition(&fragment_defs, selection)?;
            let Some(normalized_type_condition) =
                NormalizedTypeCondition::from_type_name(type_condition.clone(), schema)?
            else {
                bail!("Unexpected empty type condition for the entity type: {type_condition}")
            };
            let context = ResponseShapeContext {
                schema: schema.clone(),
                fragment_defs: fragment_defs.clone(),
                parent_type: type_condition.clone(),
                type_condition: normalized_type_condition,
                inherited_clause: Clause::default(), // empty
                current_clause: Clause::default(),   // empty
                skip_introspection: false,           // false by default
            };
            let mut response_shape = ResponseShape::new(type_condition);
            context.process_selection(&mut response_shape, selection)?;
            Ok(response_shape)
        })
        .collect()
}

fn get_fragment_type_condition(
    fragment_defs: &Arc<FragmentMap>,
    selection: &Selection,
) -> Result<Name, FederationError> {
    Ok(match selection {
        Selection::FragmentSpread(fragment_spread) => {
            let fragment_def =
                get_fragment_definition(fragment_defs, &fragment_spread.fragment_name)?;
            fragment_def.type_condition().clone()
        }
        Selection::InlineFragment(inline) => {
            let Some(type_condition) = &inline.type_condition else {
                bail!(
                    "Expected a type condition on the inline fragment under the `_entities` selection"
                )
            };
            type_condition.clone()
        }
        _ => bail!("Expected a fragment under the `_entities` selection"),
    })
}

/// Used for field sets like `@key`/`@requires` fields.
pub fn compute_response_shape_for_selection_set(
    schema: &ValidFederationSchema,
    selection_set: &SelectionSet,
) -> Result<ResponseShape, FederationError> {
    let type_condition = &selection_set.ty;
    let Some(normalized_type_condition) =
        NormalizedTypeCondition::from_type_name(type_condition.clone(), schema)?
    else {
        bail!("Unexpected empty type condition for field set: {type_condition}")
    };
    let context = ResponseShapeContext {
        schema: schema.clone(),
        fragment_defs: Default::default(), // empty
        parent_type: type_condition.clone(),
        type_condition: normalized_type_condition,
        inherited_clause: Clause::default(), // empty
        current_clause: Clause::default(),   // empty
        skip_introspection: false,           // false by default
    };
    context.process_selection_set(selection_set)
}

//==================================================================================================
// ResponseShape display
// - This section is only for display and thus untrusted.

impl fmt::Display for DisplayTypeCondition {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if self.0.is_empty() {
            return write!(f, "<deduced>");
        }
        for (i, cond) in self.0.iter().enumerate() {
            if i > 0 {
                write!(f, " ∩ ")?;
            }
            write!(f, "{}", cond.type_name())?;
        }
        Ok(())
    }
}

impl fmt::Display for NormalizedTypeCondition {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if self.ground_set.is_empty() {
            return Err(fmt::Error);
        }

        write!(f, "{}", self.for_display)?;
        if self.for_display.0.len() != 1 {
            write!(f, " = {{")?;
            for (i, ty) in self.ground_set.iter().enumerate() {
                if i > 0 {
                    write!(f, ", ")?;
                }
                write!(f, "{}", ty.type_name)?;
            }
            write!(f, "}}")?;
        }
        Ok(())
    }
}

impl fmt::Display for Clause {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if self.0.is_empty() {
            write!(f, "true")
        } else {
            for (i, l) in self.0.iter().enumerate() {
                if i > 0 {
                    write!(f, " ∧ ")?;
                }
                match l {
                    Literal::Pos(v) => write!(f, "{v}")?,
                    Literal::Neg(v) => write!(f, "¬{v}")?,
                }
            }
            Ok(())
        }
    }
}

impl DefinitionVariant {
    fn write_indented(&self, state: &mut display_helpers::State<'_, '_>) -> fmt::Result {
        let field_display = &self.representative_field;
        let boolean_str = if !self.boolean_clause.is_always_true() {
            format!(" if {}", self.boolean_clause)
        } else {
            "".to_string()
        };
        state.write(format_args!("{field_display} (on <type>){boolean_str}"))?;
        if let Some(sub_selection_response_shape) = &self.sub_selection_response_shape {
            state.write(" ")?;
            sub_selection_response_shape.write_indented(state)?;
        }
        Ok(())
    }
}

impl fmt::Display for DefinitionVariant {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.write_indented(&mut display_helpers::State::new(f))
    }
}

impl PossibleDefinitionsPerTypeCondition {
    fn has_boolean_conditions(&self) -> bool {
        self.conditional_variants.len() > 1
            || self
                .conditional_variants
                .first()
                .is_some_and(|variant| !variant.boolean_clause.is_always_true())
    }

    fn write_indented(&self, state: &mut display_helpers::State<'_, '_>) -> fmt::Result {
        for (i, variant) in self.conditional_variants.iter().enumerate() {
            if i > 0 {
                state.new_line()?;
            }
            variant.write_indented(state)?;
        }
        Ok(())
    }
}

impl fmt::Display for PossibleDefinitionsPerTypeCondition {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.write_indented(&mut display_helpers::State::new(f))
    }
}

impl PossibleDefinitions {
    /// Is conditional on runtime type?
    fn has_type_conditions(&self, default_type_condition: &Name) -> bool {
        self.0.len() > 1
            || self.0.first().is_some_and(|(type_condition, _)| {
                !type_condition.is_named_type(default_type_condition)
            })
    }

    /// Has multiple possible definitions or has any boolean conditions?
    /// Note: This method may miss a type condition. So, check `has_type_conditions` as well.
    fn has_multiple_definitions(&self) -> bool {
        self.0.len() > 1
            || self
                .0
                .first()
                .is_some_and(|(_, per_type_cond)| per_type_cond.has_boolean_conditions())
    }

    fn write_indented(&self, state: &mut display_helpers::State<'_, '_>) -> fmt::Result {
        let arrow_sym = if self.has_multiple_definitions() {
            "-may->"
        } else {
            "----->"
        };
        let mut is_first = true;
        for (type_condition, per_type_cond) in &self.0 {
            for variant in &per_type_cond.conditional_variants {
                let field_display = &variant.representative_field;
                let type_cond_str = format!(" on {type_condition}");
                let boolean_str = if !variant.boolean_clause.is_always_true() {
                    format!(" if {}", variant.boolean_clause)
                } else {
                    "".to_string()
                };
                if is_first {
                    is_first = false;
                } else {
                    state.new_line()?;
                }
                state.write(format_args!(
                    "{arrow_sym} {field_display}{type_cond_str}{boolean_str}"
                ))?;
                if let Some(sub_selection_response_shape) = &variant.sub_selection_response_shape {
                    state.write(" ")?;
                    sub_selection_response_shape.write_indented(state)?;
                }
            }
        }
        Ok(())
    }
}

impl fmt::Display for PossibleDefinitions {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.write_indented(&mut display_helpers::State::new(f))
    }
}

impl ResponseShape {
    fn write_indented(&self, state: &mut display_helpers::State<'_, '_>) -> fmt::Result {
        state.write("{")?;
        state.indent_no_new_line();
        for (response_key, defs) in &self.definitions_per_response_key {
            let has_type_cond = defs.has_type_conditions(&self.default_type_condition);
            let arrow_sym = if has_type_cond || defs.has_multiple_definitions() {
                "-may->"
            } else {
                "----->"
            };
            for (type_condition, per_type_cond) in &defs.0 {
                for variant in &per_type_cond.conditional_variants {
                    let field_display = &variant.representative_field;
                    let type_cond_str = if has_type_cond {
                        format!(" on {type_condition}")
                    } else {
                        "".to_string()
                    };
                    let boolean_str = if !variant.boolean_clause.is_always_true() {
                        format!(" if {}", variant.boolean_clause)
                    } else {
                        "".to_string()
                    };
                    state.new_line()?;
                    state.write(format_args!(
                        "{response_key} {arrow_sym} {field_display}{type_cond_str}{boolean_str}"
                    ))?;
                    if let Some(sub_selection_response_shape) =
                        &variant.sub_selection_response_shape
                    {
                        state.write(" ")?;
                        sub_selection_response_shape.write_indented(state)?;
                    }
                }
            }
        }
        state.dedent()?;
        state.write("}")
    }
}

impl fmt::Display for ResponseShape {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.write_indented(&mut display_helpers::State::new(f))
    }
}
