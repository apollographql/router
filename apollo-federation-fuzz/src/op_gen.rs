//! Layer 3: generate valid operations against an api-schema string using
//! `apollo-smith`, then optionally decorate them with operation variables +
//! `@skip` / `@include` (and optionally `@defer`) via `apollo-compiler`'s AST.
//!
//! Why decorate? `apollo-smith` produces structurally valid operations but
//! does not exercise the directive-conditioned-selection surface where
//! historical planner bugs live (e.g. FED-505: missing ConditionNode for
//! `@skip`/`@include` on interface implementations). Post-processing the
//! generated AST lets us probe that surface without forking apollo-smith.

use std::collections::BTreeSet;

use apollo_compiler::ExecutableDocument;
use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast;
use apollo_compiler::validation::Valid;
use apollo_parser::Parser;
use apollo_smith::{Document, DocumentBuilder};
use arbitrary::{Arbitrary, Unstructured};

#[derive(Debug)]
pub enum OpGenError {
    SchemaParse(String),
    SchemaValidate(String),
    Generator(String),
    Validate(String),
    Decorate(String),
}

impl std::fmt::Display for OpGenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SchemaParse(s) => write!(f, "schema parse: {s}"),
            Self::SchemaValidate(s) => write!(f, "schema validate: {s}"),
            Self::Generator(s) => write!(f, "smith generator: {s}"),
            Self::Validate(s) => write!(f, "operation validate: {s}"),
            Self::Decorate(s) => write!(f, "operation decorate: {s}"),
        }
    }
}

impl std::error::Error for OpGenError {}

/// Knobs for the post-`apollo-smith` decorator.
#[derive(Debug, Clone)]
pub struct OpGenConfig {
    /// Maximum number of `Boolean!` variables to declare. Each emitted
    /// `@skip`/`@include` references one of these, so a value of 0 disables
    /// the whole decorator.
    pub max_vars: u32,
    /// Probability (0..=255) that any individual selection gets a
    /// `@skip(if:)` or `@include(if:)` decoration, conditional on
    /// `max_vars > 0`.
    pub skip_include_chance: u8,
    /// Probability (0..=255) that any individual inline-fragment selection
    /// gets a `@defer` decoration. The planner only honours `@defer` when
    /// `CommonConfig::incremental_delivery = true`; otherwise the directive
    /// is accepted but produces no `DeferNode`.
    pub defer_chance: u8,
    /// Probability (0..=255) that a non-empty selection set gets an
    /// extra `__typename` field appended. Skipped if `__typename` is
    /// already present at that position. Pokes FED-251 (`__typename`
    /// mishandling) territory.
    pub typename_sprinkle_chance: u8,
    /// Probability (0..=255) that a leaf field selection gets duplicated
    /// in place with a fresh alias and a `@skip(if: $v)` on the
    /// duplicate. Forces the planner to deal with two paths to the same
    /// underlying field, one runtime-conditional. Requires `max_vars > 0`.
    pub alias_skip_chance: u8,
    /// Probability (0..=255) that an eligible inline fragment is lifted
    /// into a named `FragmentDefinition` with the original site replaced
    /// by a `FragmentSpread`. Eligibility: the fragment has a type
    /// condition and a non-empty selection set. Targets fragment-
    /// normalization regression territory the existing harness has zero
    /// coverage of (apollo-smith never produces named definitions).
    pub fragment_extraction_chance: u8,
}

impl Default for OpGenConfig {
    fn default() -> Self {
        Self {
            max_vars: 2,
            skip_include_chance: 80,
            defer_chance: 0,
            typename_sprinkle_chance: 50,
            alias_skip_chance: 60,
            fragment_extraction_chance: 80,
        }
    }
}

/// Generate one operation document from the bytes in `seed`. With a default
/// [`OpGenConfig`] the result is decorated with `@skip`/`@include` + their
/// associated `Boolean!` variables. Returns `Err` when apollo-smith produces
/// nothing, or when the resulting (decorated) operation fails validation.
pub fn generate_operation(api_schema_sdl: &str, seed: &[u8]) -> Result<String, OpGenError> {
    generate_operation_with_config(api_schema_sdl, seed, &OpGenConfig::default())
}

pub fn generate_operation_with_config(
    api_schema_sdl: &str,
    seed: &[u8],
    cfg: &OpGenConfig,
) -> Result<String, OpGenError> {
    let parsed = Parser::new(api_schema_sdl).parse();
    if parsed.errors().next().is_some() {
        let detail = parsed
            .errors()
            .map(|e| e.message().to_string())
            .collect::<Vec<_>>()
            .join("; ");
        return Err(OpGenError::SchemaParse(detail));
    }
    let smith_doc: Document = parsed
        .document()
        .try_into()
        .map_err(|e: apollo_smith::FromError| OpGenError::SchemaParse(e.to_string()))?;

    // Apollo-smith's `DocumentBuilder` holds a mutable borrow on `u` for its
    // entire lifetime, so we use a fresh `Unstructured` over the second half
    // of the seed for the decorator. Splitting the seed keeps everything
    // deterministic relative to the original input.
    let split = seed.len() / 2;
    let (smith_seed, decorator_seed) = seed.split_at(split.max(1));

    let mut smith_u = Unstructured::new(smith_seed);
    let mut builder = DocumentBuilder::with_document(&mut smith_u, smith_doc)
        .map_err(|e| OpGenError::Generator(e.to_string()))?;
    let op = builder
        .operation_definition()
        .map_err(|e| OpGenError::Generator(e.to_string()))?
        .ok_or_else(|| OpGenError::Generator("no operation produced".to_string()))?;
    let op_text: String = op.into();
    drop(builder);

    let mut decorator_u = Unstructured::new(decorator_seed);
    let decorated = decorate_operation(&op_text, &mut decorator_u, cfg)
        .map_err(|e| OpGenError::Decorate(e.to_string()))?;

    // Composed federation supergraphs don't declare `@defer` (the planner
    // augments its internal schema when `incremental_delivery` is on, but
    // the SDL we hand to op-gen for validation is the raw composed form).
    // Inject the directive declaration locally so apollo-compiler's
    // validator accepts decorated operations. Harmless when no `@defer`
    // was emitted.
    let schema_for_validation: String = if cfg.defer_chance > 0 {
        format!(
            "directive @defer(if: Boolean! = true, label: String) on FRAGMENT_SPREAD | INLINE_FRAGMENT\n\n{api_schema_sdl}"
        )
    } else {
        api_schema_sdl.to_string()
    };
    let valid_schema =
        apollo_compiler::Schema::parse_and_validate(&schema_for_validation, "api.graphql")
            .map_err(|e| OpGenError::SchemaValidate(e.to_string()))?;
    let _doc: Valid<ExecutableDocument> =
        ExecutableDocument::parse_and_validate(&valid_schema, &decorated, "operation.graphql")
            .map_err(|e| OpGenError::Validate(e.to_string()))?;

    Ok(decorated)
}

fn decorate_operation(
    op_text: &str,
    u: &mut Unstructured,
    cfg: &OpGenConfig,
) -> Result<String, String> {
    let any_pass_active = cfg.max_vars != 0
        || cfg.defer_chance != 0
        || cfg.typename_sprinkle_chance != 0
        || cfg.alias_skip_chance != 0
        || cfg.fragment_extraction_chance != 0;
    if !any_pass_active {
        return Ok(op_text.to_string());
    }

    let mut doc = ast::Document::parse(op_text, "op.graphql")
        .map_err(|e| format!("parse: {e}"))?;

    let mut used_vars: BTreeSet<u32> = BTreeSet::new();
    for def in doc.definitions.iter_mut() {
        if let ast::Definition::OperationDefinition(op_node) = def {
            let op = op_node.make_mut();
            decorate_selections(&mut op.selection_set, cfg, u, &mut used_vars)
                .map_err(|e| format!("walk: {e}"))?;
            let mut alias_counter: u32 = 0;
            shape_selections(
                &mut op.selection_set,
                cfg,
                u,
                &mut used_vars,
                &mut alias_counter,
            )
            .map_err(|e| format!("shape: {e}"))?;
            for v_idx in &used_vars {
                op.variables.push(Node::new(ast::VariableDefinition {
                    name: var_name(*v_idx),
                    ty: Node::new(ast::Type::NonNullNamed(boolean_type_name())),
                    default_value: None,
                    directives: ast::DirectiveList::default(),
                }));
            }
        }
    }

    if cfg.fragment_extraction_chance > 0 {
        extract_fragments(&mut doc, cfg, u).map_err(|e| format!("extract: {e}"))?;
    }

    Ok(doc.to_string())
}

fn decorate_selections(
    selections: &mut [ast::Selection],
    cfg: &OpGenConfig,
    u: &mut Unstructured,
    used_vars: &mut BTreeSet<u32>,
) -> arbitrary::Result<()> {
    for sel in selections.iter_mut() {
        // @skip / @include
        if cfg.max_vars > 0 && cfg.skip_include_chance > 0 {
            let roll: u8 = u8::arbitrary(u)?;
            if roll < cfg.skip_include_chance {
                let v_idx = u.int_in_range(0..=cfg.max_vars - 1)?;
                used_vars.insert(v_idx);
                let directive_name = if bool::arbitrary(u)? {
                    "skip"
                } else {
                    "include"
                };
                let directive = ast::Directive {
                    name: Name::new(directive_name).expect("static name"),
                    arguments: vec![Node::new(ast::Argument {
                        name: Name::new("if").expect("static name"),
                        value: Node::new(ast::Value::Variable(var_name(v_idx))),
                    })],
                };
                push_directive(sel, directive);
            }
        }

        // @defer (only legal on inline fragments and fragment spreads).
        if let (true, ast::Selection::InlineFragment(_) | ast::Selection::FragmentSpread(_)) = (
            cfg.defer_chance > 0 && u8::arbitrary(u)? < cfg.defer_chance,
            &*sel,
        ) {
            let directive = ast::Directive {
                name: Name::new("defer").expect("static name"),
                arguments: vec![],
            };
            push_directive(sel, directive);
        }

        // Recurse into nested selection sets.
        match sel {
            ast::Selection::Field(f) => {
                decorate_selections(&mut f.make_mut().selection_set, cfg, u, used_vars)?;
            }
            ast::Selection::InlineFragment(f) => {
                decorate_selections(&mut f.make_mut().selection_set, cfg, u, used_vars)?;
            }
            ast::Selection::FragmentSpread(_) => {}
        }
    }
    Ok(())
}

/// Phase A shape-mutation pass. Run *after* `decorate_selections` so the
/// directive decorations land on whatever exists at that point; the new
/// duplicates we introduce here either get their own `@skip(if: $v)` or
/// stay clean.
///
/// Two transformations:
///
/// 1. `typename_sprinkle_chance`: at any non-empty selection set that
///    doesn't already select `__typename`, append a `__typename` field.
/// 2. `alias_skip_chance`: any leaf scalar field selection (no
///    sub-selection) gets a sibling alias-duplicate with `@skip(if: $v)`.
fn shape_selections(
    selections: &mut Vec<ast::Selection>,
    cfg: &OpGenConfig,
    u: &mut Unstructured,
    used_vars: &mut BTreeSet<u32>,
    alias_counter: &mut u32,
) -> arbitrary::Result<()> {
    if cfg.typename_sprinkle_chance > 0
        && !selections.iter().any(|s| matches!(s, ast::Selection::Field(f)
            if f.name.as_str() == "__typename"))
        && !selections.is_empty()
        && u8::arbitrary(u)? < cfg.typename_sprinkle_chance
    {
        selections.push(ast::Selection::Field(Node::new(ast::Field {
            alias: None,
            name: Name::new("__typename").expect("static name"),
            arguments: Vec::new(),
            directives: ast::DirectiveList::default(),
            selection_set: Vec::new(),
        })));
    }

    // Alias-with-skip duplication. Runs only on leaf scalar fields with
    // no existing alias, to avoid messing up nested response shapes.
    let mut alias_inserts: Vec<(usize, ast::Selection)> = Vec::new();
    if cfg.alias_skip_chance > 0 && cfg.max_vars > 0 {
        for (idx, sel) in selections.iter().enumerate() {
            if let ast::Selection::Field(field_node) = sel {
                let field: &ast::Field = field_node;
                if field.alias.is_none()
                    && field.selection_set.is_empty()
                    && !field.name.as_str().starts_with("__")
                    && u8::arbitrary(u)? < cfg.alias_skip_chance
                {
                    let v_idx = u.int_in_range(0..=cfg.max_vars - 1)?;
                    used_vars.insert(v_idx);
                    let alias_name = format!("aS{}", *alias_counter);
                    *alias_counter += 1;
                    let mut clone = field.clone();
                    clone.alias = Some(Name::new(&alias_name).expect("alias is valid name"));
                    // Strip prior `@skip`/`@include` from the duplicate so
                    // the GraphQL non-repeatable rule isn't violated when
                    // the original carried one. The duplicate always gets
                    // a fresh `@skip(if: $v)`; that's the whole point.
                    clone
                        .directives
                        .retain(|d| !matches!(d.name.as_str(), "skip" | "include"));
                    clone.directives.push(Node::new(ast::Directive {
                        name: Name::new("skip").expect("static name"),
                        arguments: vec![Node::new(ast::Argument {
                            name: Name::new("if").expect("static name"),
                            value: Node::new(ast::Value::Variable(var_name(v_idx))),
                        })],
                    }));
                    alias_inserts.push((idx + 1, ast::Selection::Field(Node::new(clone))));
                }
            }
        }
    }
    // Apply inserts in reverse so each `idx + 1` stays valid.
    for (pos, sel) in alias_inserts.into_iter().rev() {
        selections.insert(pos, sel);
    }

    // Recurse.
    for sel in selections.iter_mut() {
        match sel {
            ast::Selection::Field(f) => {
                shape_selections(
                    &mut f.make_mut().selection_set,
                    cfg,
                    u,
                    used_vars,
                    alias_counter,
                )?;
            }
            ast::Selection::InlineFragment(f) => {
                shape_selections(
                    &mut f.make_mut().selection_set,
                    cfg,
                    u,
                    used_vars,
                    alias_counter,
                )?;
            }
            ast::Selection::FragmentSpread(_) => {}
        }
    }
    Ok(())
}

/// Lift some inline fragments with type conditions into named
/// `FragmentDefinition`s. The original site becomes a `FragmentSpread`
/// carrying the inline fragment's directives; the type condition + body
/// move to the definition. Skipped fragments stay inline. Apollo-smith
/// never produces named fragment definitions, so this opens a planner
/// surface (fragment normalisation) we'd otherwise miss.
fn extract_fragments(
    doc: &mut ast::Document,
    cfg: &OpGenConfig,
    u: &mut Unstructured,
) -> arbitrary::Result<()> {
    let mut next_id: u32 = 0;
    let mut new_defs: Vec<ast::Definition> = Vec::new();
    for def in doc.definitions.iter_mut() {
        if let ast::Definition::OperationDefinition(op_node) = def {
            let op = op_node.make_mut();
            walk_extract(&mut op.selection_set, cfg, u, &mut next_id, &mut new_defs)?;
        }
    }
    for nd in new_defs {
        doc.definitions.push(nd);
    }
    Ok(())
}

fn walk_extract(
    selections: &mut [ast::Selection],
    cfg: &OpGenConfig,
    u: &mut Unstructured,
    next_id: &mut u32,
    new_defs: &mut Vec<ast::Definition>,
) -> arbitrary::Result<()> {
    for sel in selections.iter_mut() {
        // Recurse first so inner fragments may be extracted before we
        // potentially extract the outer one.
        match sel {
            ast::Selection::Field(f) => {
                walk_extract(&mut f.make_mut().selection_set, cfg, u, next_id, new_defs)?;
            }
            ast::Selection::InlineFragment(f) => {
                walk_extract(&mut f.make_mut().selection_set, cfg, u, next_id, new_defs)?;
            }
            ast::Selection::FragmentSpread(_) => continue,
        }

        // Decide whether to lift this inline fragment, then materialise
        // the change. We do the inspection in a separate scope so the
        // immutable borrow of `sel` is released before we reassign it.
        let extract_data: Option<(_, _, _)> = if let ast::Selection::InlineFragment(frag_node) =
            &*sel
        {
            let frag: &ast::InlineFragment = frag_node;
            // Eligibility: type condition present, non-empty selection.
            if let Some(type_cond) = &frag.type_condition
                && !frag.selection_set.is_empty()
                && u8::arbitrary(u)? < cfg.fragment_extraction_chance
            {
                Some((
                    type_cond.clone(),
                    frag.selection_set.clone(),
                    frag.directives.clone(),
                ))
            } else {
                None
            }
        } else {
            None
        };

        if let Some((type_cond, selection_set, directives)) = extract_data {
            let frag_name =
                Name::new(&format!("Frag{}", *next_id)).expect("Frag<n> is valid name");
            *next_id += 1;

            // Build the new fragment definition. Directives stay with
            // the spread (they apply at the spread point); only the
            // type condition + selection set move to the definition.
            let definition = ast::FragmentDefinition {
                name: frag_name.clone(),
                type_condition: type_cond,
                directives: ast::DirectiveList::default(),
                selection_set,
            };
            new_defs.push(ast::Definition::FragmentDefinition(Node::new(definition)));

            // Replace the inline fragment site with a FragmentSpread.
            let spread = ast::FragmentSpread {
                fragment_name: frag_name,
                directives,
            };
            *sel = ast::Selection::FragmentSpread(Node::new(spread));
        }
    }
    Ok(())
}

fn push_directive(sel: &mut ast::Selection, d: ast::Directive) {
    let node = Node::new(d);
    match sel {
        ast::Selection::Field(f) => f.make_mut().directives.0.push(node),
        ast::Selection::InlineFragment(f) => f.make_mut().directives.0.push(node),
        ast::Selection::FragmentSpread(f) => f.make_mut().directives.0.push(node),
    }
}

fn var_name(idx: u32) -> Name {
    Name::new(&format!("__v{idx}")).expect("valid var name")
}

fn boolean_type_name() -> Name {
    Name::new("Boolean").expect("static")
}
