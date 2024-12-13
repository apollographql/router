use std::fmt;
use std::sync::Arc;

use apollo_compiler::ast;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::executable::Field;
use apollo_compiler::executable::Fragment;
use apollo_compiler::executable::Selection;
use apollo_compiler::executable::SelectionSet;
use apollo_compiler::validation::Valid;
use apollo_compiler::ExecutableDocument;
use apollo_compiler::Name;
use apollo_compiler::Node;

use crate::bail;
use crate::display_helpers;
use crate::internal_error;
use crate::schema::position::CompositeTypeDefinitionPosition;
use crate::schema::position::InterfaceTypeDefinitionPosition;
use crate::schema::position::ObjectTypeDefinitionPosition;
use crate::schema::ValidFederationSchema;
use crate::utils::FallibleIterator;
use crate::FederationError;

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
/// - All type-definition positions are in the API schema.
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
fn get_ground_types(
    ty: &CompositeTypeDefinitionPosition,
    schema: &ValidFederationSchema,
) -> Result<Vec<ObjectTypeDefinitionPosition>, FederationError> {
    let mut result = schema.possible_runtime_types(ty.clone())?;
    result.sort_by(|a, b| a.type_name.cmp(&b.type_name));
    Ok(result.into_iter().collect())
}

/// A sequence of type conditions applied (used for display)
// - The vector must be non-empty.
#[derive(Debug, Clone)]
struct AppliedTypeCondition(Vec<CompositeTypeDefinitionPosition>);

impl fmt::Display for AppliedTypeCondition {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        for (i, cond) in self.0.iter().enumerate() {
            if i > 0 {
                write!(f, " ∧ ")?;
            }
            write!(f, "{}", cond.type_name())?;
        }
        Ok(())
    }
}

impl AppliedTypeCondition {
    fn new(ty: CompositeTypeDefinitionPosition) -> Self {
        AppliedTypeCondition(vec![ty])
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
        Ok(AppliedTypeCondition(buf))
    }
}

#[derive(Debug, Clone)]
struct NormalizedTypeCondition {
    // The set of object types that are used for comparison.
    // - The ground_set must be non-empty.
    // - The ground_set must be sorted by type name.
    ground_set: Vec<ObjectTypeDefinitionPosition>,

    // Simplified type condition for display.
    for_display: AppliedTypeCondition,
}

impl PartialEq for NormalizedTypeCondition {
    fn eq(&self, other: &Self) -> bool {
        self.ground_set == other.ground_set
    }
}

impl Eq for NormalizedTypeCondition {}

impl fmt::Display for NormalizedTypeCondition {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.for_display)?;
        if self.for_display.0.len() > 1 {
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

impl std::hash::Hash for NormalizedTypeCondition {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.ground_set.hash(state);
    }
}

impl NormalizedTypeCondition {
    /// Construct a new type condition with a single named type condition.
    fn from_type_name(name: Name, schema: &ValidFederationSchema) -> Result<Self, FederationError> {
        let ty: CompositeTypeDefinitionPosition = schema.get_type(name)?.try_into()?;
        Ok(NormalizedTypeCondition {
            ground_set: get_ground_types(&ty, schema)?,
            for_display: AppliedTypeCondition::new(ty),
        })
    }

    /// Construct a new type condition with a named type condition added.
    fn add_type_name(
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
}

//==================================================================================================
// Logical conditions

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Literal {
    Pos(Name), // positive occurrence of the variable with the given name
    Neg(Name), // negated variable with the given name
}

impl Literal {
    fn variable(&self) -> &Name {
        match self {
            Literal::Pos(name) | Literal::Neg(name) => name,
        }
    }

    fn polarity(&self) -> bool {
        matches!(self, Literal::Pos(_))
    }
}

// A clause is a conjunction of literals.
// Empty Clause means "true".
// "false" can't be represented. Any cases with false condition must be dropped entirely.
// This vector must be deduplicated.
#[derive(Debug, Clone, Default, Eq)]
struct Clause(Vec<Literal>);

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
                    Literal::Pos(v) => write!(f, "{}", v)?,
                    Literal::Neg(v) => write!(f, "¬{}", v)?,
                }
            }
            Ok(())
        }
    }
}

impl Clause {
    fn is_always_true(&self) -> bool {
        self.0.is_empty()
    }

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

    fn concatenate(&self, other: &Clause) -> Option<Clause> {
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
    fn concatenate_and_simplify(&self, clause: &Clause) -> Option<(Clause, Clause)> {
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
            bail!("missing @skip(if:) argument");
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
                bail!("expected boolean or variable `if` argument, got {value}");
            }
        }
    }

    if let Some(include) = directives.get("include") {
        let Some(value) = include.specified_argument_by_name("if") else {
            bail!("missing @include(if:) argument");
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
                bail!("expected boolean or variable `if` argument, got {value}");
            }
        }
    }
    Ok(Some(Clause::from_variable_map(&variables)))
}

fn normalized_arguments(args: &[Node<ast::Argument>]) -> Vec<Node<ast::Argument>> {
    vec_sorted_by(args, |a, b| a.name.cmp(&b.name))
}

fn remove_conditions_from_directives(directives: &ast::DirectiveList) -> ast::DirectiveList {
    directives
        .iter()
        .filter(|d| d.name != "skip" && d.name != "include")
        .cloned()
        .collect()
}

type FieldSelectionKey = Field;

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
struct DefinitionVariant {
    /// Boolean clause is the secondary key after NormalizedTypeCondition as primary key.
    boolean_clause: Clause,

    /// Field selection for definition/display (see `fn field_display`).
    /// - This is the first field of the same field selection key in depth-first order as
    ///   defined by `CollectFields` and `ExecuteField` algorithms in the GraphQL spec.
    field_display: Field,

    /// Different variants can have different sets of sub-selections (if any).
    sub_selection_response_shape: Option<ResponseShape>,
}

#[derive(Debug, PartialEq, Eq, Clone)]
struct PossibleDefinitionsPerTypeCondition {
    /// The key for comparison (only used for GraphQL invariant check).
    /// - Under each type condition, all variants must have the same selection key.
    field_selection_key: FieldSelectionKey,

    /// Under each type condition, there may be multiple variants with different Boolean conditions.
    conditional_variants: Vec<DefinitionVariant>,
    // - Every variant's (Boolean condition, directive key) must be unique.
    // - (TBD) Their Boolean conditions must be mutually exclusive.
}

impl PossibleDefinitionsPerTypeCondition {
    fn insert_variant(&mut self, variant: DefinitionVariant) {
        for existing in &mut self.conditional_variants {
            if existing.boolean_clause == variant.boolean_clause {
                // Merge response shapes (MergeSelectionSets from GraphQL spec 6.4.3)
                match (
                    &mut existing.sub_selection_response_shape,
                    variant.sub_selection_response_shape,
                ) {
                    (None, None) => {} // nothing to do
                    (Some(existing_rs), Some(ref variant_rs)) => {
                        existing_rs.merge_with(variant_rs);
                    }
                    (None, Some(_)) | (Some(_), None) => {
                        unreachable!("mismatched sub-selection options")
                    }
                }
                return;
            }
        }
        self.conditional_variants.push(variant);
    }
}

/// All possible definitions that a response key can have.
/// - At the top level, all possibilities are indexed by the type condition.
/// - However, they are not necessarily mutually exclusive.
#[derive(Debug, Default, PartialEq, Eq, Clone)]
struct PossibleDefinitions(IndexMap<NormalizedTypeCondition, PossibleDefinitionsPerTypeCondition>);

impl PossibleDefinitions {
    fn insert_possible_definition(
        &mut self,
        type_conditions: NormalizedTypeCondition,
        boolean_clause: Clause, // the aggregate boolean condition of the current selection set
        field_display: Field,
        sub_selection_response_shape: Option<ResponseShape>,
    ) {
        let field_selection_key = field_selection_key(&field_display);
        let entry = self.0.entry(type_conditions);
        let insert_variant = |per_type_cond: &mut PossibleDefinitionsPerTypeCondition| {
            let value = DefinitionVariant {
                boolean_clause,
                field_display,
                sub_selection_response_shape,
            };
            per_type_cond.insert_variant(value);
        };
        match entry {
            indexmap::map::Entry::Vacant(e) => {
                // New type condition
                let empty_per_type_cond = PossibleDefinitionsPerTypeCondition {
                    field_selection_key,
                    conditional_variants: vec![],
                };
                insert_variant(e.insert(empty_per_type_cond));
            }
            indexmap::map::Entry::Occupied(mut e) => {
                // GraphQL invariant: per_type_cond.field_selection_key must be the same
                //                    as the given field_selection_key.
                assert_eq!(e.get().field_selection_key, field_selection_key);
                insert_variant(e.get_mut());
            }
        };
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct ResponseShape {
    /// The default type condition is only used for display.
    default_type_condition: NormalizedTypeCondition,
    definitions_per_response_key: IndexMap</*response_key*/ Name, PossibleDefinitions>,
}

impl ResponseShape {
    fn new(default_type_condition: NormalizedTypeCondition) -> Self {
        ResponseShape {
            default_type_condition,
            definitions_per_response_key: IndexMap::default(),
        }
    }

    fn merge_with(&mut self, other: &Self) {
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
                        variant.field_display.clone(),
                        variant.sub_selection_response_shape.clone(),
                    );
                }
            }
        }
    }
}

//==================================================================================================
// ResponseShape display

impl PossibleDefinitionsPerTypeCondition {
    fn has_boolean_conditions(&self) -> bool {
        self.conditional_variants.len() > 1
            || self
                .conditional_variants
                .first()
                .is_some_and(|variant| !variant.boolean_clause.is_always_true())
    }
}

impl PossibleDefinitions {
    /// Is conditional on runtime type?
    fn has_type_conditions(&self, default_type_condition: &NormalizedTypeCondition) -> bool {
        self.0.len() > 1
            || self
                .0
                .first()
                .is_some_and(|(type_condition, _)| type_condition != default_type_condition)
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
                    let field_display = &variant.field_display;
                    let type_cond_str = if has_type_cond {
                        format!(" on {}", type_condition)
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

//==================================================================================================
// ResponseShape computation

struct ResponseShapeContext {
    schema: ValidFederationSchema,
    fragment_defs: Arc<IndexMap<Name, Node<Fragment>>>, // fragment definitions in the operation
    parent_type: Name,                                  // the type of the current selection set
    type_condition: NormalizedTypeCondition, // accumulated type condition down from the parent field.
    inherited_clause: Clause, // accumulated conditions from the root up to parent field
    current_clause: Clause,   // accumulated conditions down from the parent field
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
                let fragment_def = self
                    .fragment_defs
                    .get(&fragment_spread.fragment_name)
                    .ok_or_else(|| {
                        internal_error!("Fragment not found: {}", fragment_spread.fragment_name)
                    })?;
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
            // internal invariant check
            assert_eq!(*field.ty().inner_named_type(), field.selection_set.ty);

            // A brand new context with the new type condition.
            // - Still inherits the boolean conditions for simplification purposes.
            let parent_type = field.selection_set.ty.clone();
            let type_condition =
                NormalizedTypeCondition::from_type_name(parent_type.clone(), &self.schema)?;
            let context = ResponseShapeContext {
                schema: self.schema.clone(),
                fragment_defs: self.fragment_defs.clone(),
                parent_type,
                type_condition,
                inherited_clause,
                current_clause: Clause::default(), // empty
            };
            Some(context.process_selection_set(&field.selection_set)?)
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
        );
        Ok(())
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
        assert_eq!(*fragment_type_condition, selection_set.ty);

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
        let type_condition =
            NormalizedTypeCondition::from_type_name(selection_set.ty.clone(), &self.schema)?;
        let mut response_shape = ResponseShape::new(type_condition);
        self.process_selection_set_within(&mut response_shape, selection_set)?;
        Ok(response_shape)
    }

    fn process_operation(
        operation_doc: &Valid<ExecutableDocument>,
        schema: &ValidFederationSchema,
    ) -> Result<ResponseShape, FederationError> {
        let mut op_iter = operation_doc.operations.iter();
        let Some(first) = op_iter.next() else {
            return Err(internal_error!("Operation not found"));
        };
        if op_iter.next().is_some() {
            return Err(internal_error!("Multiple operations are not supported"));
        }

        let fragment_defs = Arc::new(operation_doc.fragments.clone());
        let parent_type = first.selection_set.ty.clone();
        let type_condition = NormalizedTypeCondition::from_type_name(parent_type.clone(), schema)?;
        // Start a new root context.
        // - Not using `process_selection_set` because there is no parent context.
        let context = ResponseShapeContext {
            schema: schema.clone(),
            fragment_defs,
            parent_type,
            type_condition,
            inherited_clause: Clause::default(), // empty
            current_clause: Clause::default(),   // empty
        };
        context.process_selection_set(&first.selection_set)
    }
}

pub fn compute_response_shape(
    operation_doc: &Valid<ExecutableDocument>,
    schema: &ValidFederationSchema,
) -> Result<ResponseShape, FederationError> {
    ResponseShapeContext::process_operation(operation_doc, schema)
}
