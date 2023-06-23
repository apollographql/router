use std::collections::HashMap;
use std::fmt;

use apollo_compiler::hir;
use apollo_compiler::ApolloCompiler;
use apollo_compiler::FileId;
use apollo_compiler::HirDatabase;
use indexmap::IndexSet;
use tower::BoxError;

use super::transform;
use super::traverse;
use super::Query;
use super::SubSelection;
use super::SubSelections;
use crate::json_ext::Path;
use crate::json_ext::PathElement;
use crate::query_planner::reconstruct_full_query;
use crate::query_planner::OperationKind;
use crate::spec::Schema;
use crate::spec::SpecError;
use crate::Configuration;

const DEFER_DIRECTIVE_NAME: &str = "defer";
const IF_ARGUMENT_NAME: &str = "if";

/// We generate subselections for all 2^N possible combinations of these boolean variables.
/// Refuse to do so for a number of combinatinons we deem unreasonable.
const MAX_DEFER_VARIABLES: usize = 4;

pub(crate) fn collect_subselections(
    configuration: &Configuration,
    schema: &Schema,
    compiler: &ApolloCompiler,
    file_id: FileId,
    kind: OperationKind,
) -> Result<(SubSelections, IndexSet<String>), SpecError> {
    if !configuration.supergraph.defer_support {
        return Ok((SubSelections::new(), IndexSet::new()));
    }
    let (keys, defer_variables_set) = subselection_keys(configuration, schema, compiler, file_id)
        .map_err(|e| SpecError::ParsingError(e.to_string()))?;
    keys.into_iter()
        .map(|(key, path, subselection)| {
            let reconstructed = reconstruct_full_query(&path, &kind, &subselection);
            let value = Query::parse(reconstructed, schema, &Default::default())?;
            Ok((key, value))
        })
        .collect::<Result<SubSelections, SpecError>>()
        .map(|keys| (keys, defer_variables_set))
}

/// Generate the keys of the eventual `Query::subselections` hashmap.
///
/// They should be identical to paths and `subselection` strings found in
/// `.primary` and `.deferred[i]` of `Defer` nodes of the query plan.
#[allow(clippy::type_complexity)]
fn subselection_keys(
    configuration: &Configuration,
    schema: &Schema,
    compiler: &ApolloCompiler,
    file_id: FileId,
) -> Result<(Vec<(SubSelection, Path, String)>, IndexSet<String>), BoxError> {
    let HasDefer {
        has_defer,
        has_unconditional_defer,
    } = has_defer(compiler, file_id)?;
    if !has_defer {
        return Ok((Vec::new(), IndexSet::new()));
    }
    let inlined = transform_fragment_spreads_to_inline_fragments(compiler, file_id)?.to_string();
    let (compiler, file_id) = Query::make_compiler(&inlined, schema, configuration);
    let variables = conditional_defer_variable_names(&compiler, file_id)?;
    if variables.len() > MAX_DEFER_VARIABLES {
        return Err("@defer conditional on too many different variables".into());
    }
    let mut keys = Vec::new();
    for combination in variable_combinations(&variables, has_unconditional_defer) {
        collect_subselections_keys(&compiler, file_id, combination, &mut keys)?
    }
    Ok((keys, variables))
}

struct HasDefer {
    /// Whether @defer is used at all
    has_defer: bool,
    /// Whether @defer is used at least once without an `if` argument (or with `if: true`)
    has_unconditional_defer: bool,
}

fn has_defer(compiler: &ApolloCompiler, file_id: FileId) -> Result<HasDefer, BoxError> {
    struct Visitor<'a> {
        compiler: &'a ApolloCompiler,
        results: HasDefer,
    }

    impl traverse::Visitor for Visitor<'_> {
        fn compiler(&self) -> &apollo_compiler::ApolloCompiler {
            self.compiler
        }

        fn fragment_spread(&mut self, hir: &hir::FragmentSpread) -> Result<(), BoxError> {
            self.check(hir.directive_by_name(DEFER_DIRECTIVE_NAME))?;
            traverse::fragment_spread(self, hir)
        }

        fn inline_fragment(
            &mut self,
            parent_type: &str,
            hir: &hir::InlineFragment,
        ) -> Result<(), BoxError> {
            self.check(hir.directive_by_name(DEFER_DIRECTIVE_NAME))?;
            traverse::inline_fragment(self, parent_type, hir)
        }
    }

    impl Visitor<'_> {
        fn check(&mut self, directive: Option<&hir::Directive>) -> Result<(), BoxError> {
            if let Some(directive) = directive {
                match directive.argument_by_name(IF_ARGUMENT_NAME) {
                    None => {
                        // TODO: No need to keep traversing. Visitor with early exit?
                        self.results.has_unconditional_defer = true;
                        self.results.has_defer = true;
                    }
                    Some(hir::Value::Boolean { value, .. }) if *value => {
                        // TODO: No need to keep traversing. Visitor with early exit?
                        self.results.has_unconditional_defer = true;
                        self.results.has_defer = true;
                    }
                    Some(hir::Value::Boolean { .. }) => {}
                    Some(hir::Value::Variable(_)) => self.results.has_defer = true,
                    Some(_) => return Err("non-boolean `if` argument for `@defer`".into()),
                }
            }
            Ok(())
        }
    }

    let mut visitor = Visitor {
        compiler,
        results: HasDefer {
            has_defer: false,
            has_unconditional_defer: false,
        },
    };
    traverse::document(&mut visitor, file_id)?;
    Ok(visitor.results)
}

fn transform_fragment_spreads_to_inline_fragments(
    compiler: &ApolloCompiler,
    file_id: FileId,
) -> Result<apollo_encoder::Document, BoxError> {
    struct Visitor<'a> {
        compiler: &'a ApolloCompiler,
        cache: HashMap<String, Result<Option<apollo_encoder::Selection>, String>>,
    }

    impl<'a> transform::Visitor for Visitor<'a> {
        fn compiler(&self) -> &apollo_compiler::ApolloCompiler {
            self.compiler
        }

        fn fragment_definition(
            &mut self,
            _hir: &hir::FragmentDefinition,
        ) -> Result<Option<apollo_encoder::FragmentDefinition>, BoxError> {
            Ok(None)
        }

        fn selection(
            &mut self,
            hir: &hir::Selection,
            parent_type: &str,
        ) -> Result<Option<apollo_encoder::Selection>, BoxError> {
            match hir {
                hir::Selection::FragmentSpread(fragment_spread) => {
                    let name = fragment_spread.name();
                    if let Some(result) = self.cache.get(name) {
                        return Ok(result.clone()?);
                    }
                    let result = convert(self, fragment_spread);
                    self.cache.insert(name.into(), result.clone());
                    Ok(result?)
                }
                _ => transform::selection(self, hir, parent_type),
            }
        }
    }

    fn convert(
        visitor: &mut Visitor<'_>,
        fragment_spread: &hir::FragmentSpread,
    ) -> Result<Option<apollo_encoder::Selection>, String> {
        let fragment_def = fragment_spread
            .fragment(&visitor.compiler.db)
            .ok_or("Missing fragment definition")?;

        let parent_type = fragment_def.type_condition();
        let result = transform::selection_set(visitor, fragment_def.selection_set(), parent_type);
        let Some(selection_set) = result.map_err(|e| e.to_string())?
        else { return Ok(None) };

        let mut encoder_node = apollo_encoder::InlineFragment::new(selection_set);

        encoder_node.type_condition(Some(apollo_encoder::TypeCondition::new(
            fragment_def.type_condition().into(),
        )));

        for hir in fragment_spread.directives() {
            if let Some(d) = transform::directive(hir).map_err(|e| e.to_string())? {
                encoder_node.directive(d)
            }
        }
        Ok(Some(apollo_encoder::Selection::InlineFragment(
            encoder_node,
        )))
    }

    let mut visitor = Visitor {
        compiler,
        cache: HashMap::new(),
    };
    transform::document(&mut visitor, file_id)
}

/// Return the names of boolean variables used in conditional defer like `@defer(if=$example)`
fn conditional_defer_variable_names(
    compiler: &ApolloCompiler,
    file_id: FileId,
) -> Result<IndexSet<String>, BoxError> {
    struct Visitor<'a> {
        compiler: &'a ApolloCompiler,
        variable_names: IndexSet<String>,
    }

    impl traverse::Visitor for Visitor<'_> {
        fn compiler(&self) -> &apollo_compiler::ApolloCompiler {
            self.compiler
        }

        fn fragment_spread(&mut self, hir: &hir::FragmentSpread) -> Result<(), BoxError> {
            self.collect(hir.directive_by_name(DEFER_DIRECTIVE_NAME));
            traverse::fragment_spread(self, hir)
        }

        fn inline_fragment(
            &mut self,
            parent_type: &str,
            hir: &hir::InlineFragment,
        ) -> Result<(), BoxError> {
            self.collect(hir.directive_by_name(DEFER_DIRECTIVE_NAME));
            traverse::inline_fragment(self, parent_type, hir)
        }
    }

    impl Visitor<'_> {
        fn collect(&mut self, directive: Option<&hir::Directive>) {
            if let Some(directive) = directive {
                if let Some(hir::Value::Variable(variable)) =
                    directive.argument_by_name(IF_ARGUMENT_NAME)
                {
                    self.variable_names.insert(variable.name().into());
                }
            }
        }
    }

    let mut visitor = Visitor {
        compiler,
        variable_names: IndexSet::new(),
    };
    traverse::document(&mut visitor, file_id)?;
    Ok(visitor.variable_names)
}

/// Returns an iterator of functions, one per combination of boolean values of the given variables.
/// The function return whether a given variable (by its name) is true in that combination.
fn variable_combinations(
    variables: &IndexSet<String>,
    has_unconditional_defer: bool,
) -> impl Iterator<Item = Combination> {
    // `N = variables.len()` boolean values have a total of 2^N combinations.
    // If we enumerate them by counting from 0 to 2^N - 1,
    // interpreting the N bits of the binary representation of the counter
    // as separate boolean values yields all combinations.
    // Indices within the `IndexSet` are integers from 0 to N-1,
    // and so can be used as bit offset within the counter.

    let combinations_count = 1 << variables.len();
    let initial = if has_unconditional_defer {
        // Include the `bits == 0` case where all boolean variables are false.
        // We’ll still generate subselections for remaining (unconditional) @defer
        0
    } else {
        // Exclude that case, because it doesn’t have @defer at all
        1
    };
    let combinations = initial..combinations_count;
    combinations.map(move |bits| {
        Combination { variables, bits }
        /*(bits, move |name: &str| {
            let index = variables.get_index_of(name).unwrap();
            (bits & (1 << index)) != 0
        })*/
    })
}

struct Combination<'a> {
    variables: &'a IndexSet<String>,
    bits: i32,
}

impl<'a> Combination<'a> {
    fn is_present(&self, variable: &str) -> bool {
        let index = match self.variables.get_index_of(variable) {
            Some(index) => index,
            None => return false,
        };
        let res = (self.bits & (1 << index)) != 0;
        println!("is variable {variable} present in {}: {res}", self.bits);
        res
    }

    fn bitset(&self) -> i32 {
        self.bits
    }
}

fn collect_subselections_keys(
    compiler: &ApolloCompiler,
    file_id: FileId,
    combination: Combination,
    subselection_keys: &mut Vec<(SubSelection, Path, String)>,
) -> Result<(), BoxError> {
    struct Visitor<'a> {
        combination: Combination<'a>,
        current_path: Path,
        subselection_keys: &'a mut Vec<(SubSelection, Path, String)>,
    }

    fn add_key(visitor: &mut Visitor<'_>, label: Option<String>, subselection: SelectionSet) {
        visitor.subselection_keys.push((
            SubSelection {
                label,
                variables_set: visitor.combination.bitset(),
            },
            visitor.current_path.clone(),
            subselection.to_string(),
        ))
    }

    fn selection_set(
        visitor: &mut Visitor<'_>,
        label: Option<String>,
        hir: &hir::SelectionSet,
    ) -> Result<Option<SelectionSet>, BoxError> {
        let mut subselection = Vec::new();
        for selection in hir.selection() {
            match selection {
                hir::Selection::Field(hir) => {
                    let nested = if hir.selection_set().selection().is_empty() {
                        // Leaf field
                        SelectionSet(Vec::new())
                    } else {
                        let path_element = if let Some(alias) = hir.alias() {
                            alias.name()
                        } else {
                            hir.name()
                        };
                        visitor
                            .current_path
                            .push(PathElement::Key(path_element.into()));
                        let result = selection_set(visitor, label.clone(), hir.selection_set());
                        visitor.current_path.pop();
                        if let Some(nested) = result? {
                            nested
                        } else {
                            // Every nested selection was pruned, so skip this field entirely
                            continue;
                        }
                    };
                    subselection.push(Selection::Field {
                        alias: hir.alias().map(|a| a.name().to_owned()),
                        name: hir.name().to_owned(),
                        arguments: Arguments(arguments(hir.arguments())?),
                        directives: directives(hir.directives())?,
                        selection_set: nested,
                    });
                }

                hir::Selection::InlineFragment(hir) => {
                    let is_deferred =
                        if let Some(directive) = hir.directive_by_name(DEFER_DIRECTIVE_NAME) {
                            match directive.argument_by_name(IF_ARGUMENT_NAME) {
                                None => true,
                                Some(hir::Value::Boolean { value, .. }) => *value,
                                Some(hir::Value::Variable(variable)) => {
                                    visitor.combination.is_present(variable.name())
                                }
                                _ => return Err("non-boolean `if` argument for `@defer`".into()),
                            }
                        } else {
                            // No @defer
                            false
                        };
                    let deferred_label =
                        if let Some(directive) = hir.directive_by_name(DEFER_DIRECTIVE_NAME) {
                            match directive.argument_by_name("label") {
                                None => None,
                                Some(hir::Value::String { value, .. }) => Some(value.clone()),
                                //FIXME: label could be a variable. I see no good reason to do it though, so for now this is not allowed
                                _ => return Err("non-string `label` argument for `@defer`".into()),
                            }
                        } else {
                            // No @defer
                            None
                        };

                    let type_condition = hir.type_condition();
                    if let Some(type_condition) = type_condition {
                        visitor
                            .current_path
                            .push(PathElement::Fragment(type_condition.to_string()));
                    }

                    // visitor
                    // .current_path
                    // .push(PathElement::Key(path_element.into()));
                    if is_deferred {
                        // Omit this inline fragment from `subselection`,
                        // make it a separate key instead.
                        if let Some(mut deferred) =
                            selection_set(visitor, deferred_label.clone(), hir.selection_set())?
                        {
                            if let Some(name) = hir.type_condition() {
                                deferred = SelectionSet(vec![Selection::InlineFragment {
                                    type_condition: Some(name.to_owned()),
                                    directives: Vec::new(),
                                    selection_set: deferred,
                                }]);
                            }
                            add_key(visitor, deferred_label, deferred)
                        }
                    } else if let Some(SelectionSet(mut selection_set)) =
                        selection_set(visitor, label.clone(), hir.selection_set())?
                    {
                        if let Some(name) = hir.type_condition() {
                            selection_set = vec![Selection::InlineFragment {
                                type_condition: Some(name.to_owned()),
                                directives: Vec::new(),
                                selection_set: SelectionSet(selection_set),
                            }];
                        }
                        // Non-deferred fragments appear to be flattened away
                        // in the string serialization of subselection from the query planner.
                        subselection.extend(selection_set);
                    }

                    if type_condition.is_some() {
                        visitor.current_path.pop();
                    }
                }

                // Was transformed to inline fragment earlier
                hir::Selection::FragmentSpread(_) => unreachable!(),
            }
        }
        let non_empty = !subselection.is_empty();
        Ok(if non_empty {
            Some(SelectionSet(subselection))
        } else {
            None
        })
    }

    fn arguments(hir: &[hir::Argument]) -> Result<Vec<apollo_encoder::Argument>, BoxError> {
        hir.iter()
            .map(|arg| {
                Ok(apollo_encoder::Argument::new(
                    arg.name().into(),
                    transform::value(arg.value())?,
                ))
            })
            .collect()
    }

    fn directives(hir: &[hir::Directive]) -> Result<Vec<Directive>, BoxError> {
        hir.iter()
            .map(|directive| {
                Ok(Directive {
                    name: directive.name().to_owned(),
                    arguments: Arguments(arguments(directive.arguments())?),
                })
            })
            .collect()
    }

    let mut visitor = Visitor {
        combination,
        current_path: Path::empty(),
        subselection_keys,
    };
    for hir in compiler.db.operations(file_id).iter() {
        if let Some(primary) = selection_set(&mut visitor, None, hir.selection_set())? {
            add_key(&mut visitor, None, primary)
        }
    }
    Ok(())
}

/// Similar to `apollo_encoder::SelectionSet` but with serialization matching
/// <https://github.com/apollographql/federation/blob/3299d5269/internals-js/src/operations.ts#L1823-L1851>
struct SelectionSet(Vec<Selection>);

enum Selection {
    Field {
        alias: Option<String>,
        name: String,
        arguments: Arguments,
        directives: Vec<Directive>,
        selection_set: SelectionSet,
    },
    InlineFragment {
        type_condition: Option<String>,
        directives: Vec<Directive>,
        selection_set: SelectionSet,
    },
    // FragmentSpread omitted as they’ve been transformed to inline fragments earlier
}

struct Arguments(Vec<apollo_encoder::Argument>);

struct Directive {
    name: String,
    arguments: Arguments,
}

impl fmt::Display for SelectionSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some((first, rest)) = self.0.split_first() {
            write!(f, "{{ {first}")?;
            for arg in rest {
                write!(f, " {arg}")?;
            }
            write!(f, " }}")?
        }
        Ok(())
    }
}

impl fmt::Display for Selection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Selection::Field {
                alias,
                name,
                arguments,
                directives,
                selection_set,
            } => {
                if let Some(alias) = alias {
                    write!(f, "{alias}: ")?;
                }
                write!(f, "{name}{arguments}")?;
                for directive in directives {
                    write!(f, " {directive}")?;
                }
                if !selection_set.0.is_empty() {
                    write!(f, " {selection_set}")?;
                }
            }
            Selection::InlineFragment {
                type_condition,
                directives,
                selection_set,
            } => {
                if let Some(name) = type_condition {
                    write!(f, "... on {name}")?;
                } else {
                    write!(f, "...")?;
                }
                for directive in directives {
                    write!(f, " {directive}")?;
                }
                write!(f, " {selection_set}")?;
            }
        }
        Ok(())
    }
}

impl fmt::Display for Arguments {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some((first, rest)) = self.0.split_first() {
            write!(f, "({first}")?;
            for arg in rest {
                write!(f, ", {arg}")?;
            }
            write!(f, ")")?
        }
        Ok(())
    }
}

impl fmt::Display for Directive {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "@{}{}", self.name, self.arguments)
    }
}
