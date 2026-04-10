//! Resolve supergraph element coordinates to subgraph locations for composition diagnostics.
//! Ports the behavior of `updateInaccessibleErrorsWithLinkToSubgraphs` in Apollo Federation's
//! [`merge.ts`](https://github.com/apollographql/federation/blob/f3ab499eaf62b1a1c0f08b838d2cbde5accb303a/composition-js/src/merging/merge.ts).

use apollo_compiler::Name;
use apollo_compiler::schema::ExtendedType;

use crate::error::ErrorCode;
use crate::error::HasLocations;
use crate::error::Locations;
use crate::error::SingleFederationError;
use crate::error::SubgraphLocation;
use crate::link::inaccessible_spec_definition::IsInaccessibleExt;
use crate::link::inaccessible_spec_definition::directive_uses_inaccessible;
use crate::schema::FederationSchema;
use crate::schema::position::DirectiveArgumentDefinitionPosition;
use crate::schema::position::DirectiveDefinitionPosition;
use crate::schema::position::DirectiveTargetPosition;
use crate::schema::position::EnumTypeDefinitionPosition;
use crate::schema::position::EnumValueDefinitionPosition;
use crate::schema::position::InputObjectFieldDefinitionPosition;
use crate::schema::position::InputObjectTypeDefinitionPosition;
use crate::schema::position::InterfaceFieldArgumentDefinitionPosition;
use crate::schema::position::InterfaceFieldDefinitionPosition;
use crate::schema::position::InterfaceTypeDefinitionPosition;
use crate::schema::position::ObjectFieldArgumentDefinitionPosition;
use crate::schema::position::ObjectFieldDefinitionPosition;
use crate::schema::position::ObjectTypeDefinitionPosition;
use crate::schema::position::ScalarTypeDefinitionPosition;
use crate::schema::position::UnionTypeDefinitionPosition;
use crate::subgraph::typestate::HasMetadata;
use crate::subgraph::typestate::Subgraph;

/// A coordinate string from the merged supergraph, resolved against that schema.
#[derive(Clone, Debug)]
pub(crate) enum ParsedSupergraphCoordinate {
    Target(DirectiveTargetPosition),
    DirectiveDefinition(DirectiveDefinitionPosition),
}

impl ParsedSupergraphCoordinate {
    fn exists_in(&self, schema: &FederationSchema) -> bool {
        match self {
            Self::Target(p) => p.exists_in(schema),
            Self::DirectiveDefinition(p) => p.try_get(schema.schema()).is_some(),
        }
    }

    fn locations<T: HasMetadata>(&self, subgraph: &Subgraph<T>) -> Locations {
        match self {
            Self::Target(p) => p.locations(subgraph),
            Self::DirectiveDefinition(p) => p.locations(subgraph),
        }
    }

    fn subgraph_marks_inaccessible_element(
        &self,
        _subgraph: &Subgraph<crate::subgraph::typestate::Validated>,
        inaccessible: &Name,
        schema: &FederationSchema,
    ) -> Result<bool, crate::error::FederationError> {
        match self {
            Self::Target(p) => directive_target_is_inaccessible(p, schema, inaccessible),
            Self::DirectiveDefinition(p) => {
                let def = p.get(schema.schema())?;
                Ok(directive_uses_inaccessible(inaccessible, def))
            }
        }
    }
}

fn parse_name(segment: &str) -> Option<Name> {
    Name::new(segment).ok()
}

fn type_level_target(
    schema: &FederationSchema,
    type_name: Name,
) -> Option<DirectiveTargetPosition> {
    let ext = schema.schema().types.get(&type_name)?;
    Some(match ext {
        ExtendedType::Scalar(_) => {
            DirectiveTargetPosition::ScalarType(ScalarTypeDefinitionPosition { type_name })
        }
        ExtendedType::Object(_) => {
            DirectiveTargetPosition::ObjectType(ObjectTypeDefinitionPosition { type_name })
        }
        ExtendedType::Interface(_) => {
            DirectiveTargetPosition::InterfaceType(InterfaceTypeDefinitionPosition { type_name })
        }
        ExtendedType::Union(_) => {
            DirectiveTargetPosition::UnionType(UnionTypeDefinitionPosition { type_name })
        }
        ExtendedType::Enum(_) => {
            DirectiveTargetPosition::EnumType(EnumTypeDefinitionPosition { type_name })
        }
        ExtendedType::InputObject(_) => {
            DirectiveTargetPosition::InputObjectType(InputObjectTypeDefinitionPosition {
                type_name,
            })
        }
    })
}

fn field_or_member_target(
    schema: &FederationSchema,
    type_name: Name,
    member_name: Name,
) -> Option<DirectiveTargetPosition> {
    let ext = schema.schema().types.get(&type_name)?;
    match ext {
        ExtendedType::Object(o) if o.fields.contains_key(&member_name) => Some(
            DirectiveTargetPosition::ObjectField(ObjectFieldDefinitionPosition {
                type_name,
                field_name: member_name,
            }),
        ),
        ExtendedType::Interface(i) if i.fields.contains_key(&member_name) => Some(
            DirectiveTargetPosition::InterfaceField(InterfaceFieldDefinitionPosition {
                type_name,
                field_name: member_name,
            }),
        ),
        ExtendedType::InputObject(io) if io.fields.contains_key(&member_name) => Some(
            DirectiveTargetPosition::InputObjectField(InputObjectFieldDefinitionPosition {
                type_name,
                field_name: member_name,
            }),
        ),
        ExtendedType::Enum(e) if e.values.contains_key(&member_name) => Some(
            DirectiveTargetPosition::EnumValue(EnumValueDefinitionPosition {
                type_name,
                value_name: member_name,
            }),
        ),
        _ => None,
    }
}

/// Parse a supergraph element coordinate (same string forms as [`std::fmt::Display`] on positions).
pub(crate) fn parse_supergraph_coordinate(
    schema: &FederationSchema,
    coordinate: &str,
) -> Option<ParsedSupergraphCoordinate> {
    let coordinate = coordinate.trim();
    if coordinate.is_empty() {
        return None;
    }

    // `@directive` (directive definition), e.g. `@tag`
    if coordinate.starts_with('@') && !coordinate.contains('(') && coordinate.len() > 1 {
        let directive_name = parse_name(&coordinate[1..])?;
        let pos = DirectiveDefinitionPosition { directive_name };
        return pos
            .try_get(schema.schema())
            .is_some()
            .then(|| ParsedSupergraphCoordinate::DirectiveDefinition(pos));
    }

    // `@directive(arg:)` — directive definition argument
    if coordinate.starts_with('@') {
        let rest = coordinate.strip_prefix('@')?;
        let open_paren = rest.find('(')?;
        if !rest.ends_with(')') {
            return None;
        }
        let directive_name = parse_name(&rest[..open_paren])?;
        let inner = &rest[open_paren + 1..rest.len() - 1];
        let arg_name = inner.strip_suffix(':')?;
        let arg_name = parse_name(arg_name)?;
        let pos = DirectiveArgumentDefinitionPosition {
            directive_name,
            argument_name: arg_name,
        };
        return pos.try_get(schema.schema()).is_some().then(|| {
            ParsedSupergraphCoordinate::Target(DirectiveTargetPosition::DirectiveArgument(pos))
        });
    }

    // `Type.field(arg:)` — field argument
    if let Some(open_paren) = coordinate.rfind('(')
        && coordinate.ends_with(')')
    {
        let inner = &coordinate[open_paren + 1..coordinate.len() - 1];
        if let Some(arg_name) = inner.strip_suffix(':') {
            let arg_name = parse_name(arg_name)?;
            let before = &coordinate[..open_paren];
            let dot = before.rfind('.')?;
            let type_name = parse_name(&before[..dot])?;
            let field_name = parse_name(&before[dot + 1..])?;
            let ext = schema.schema().types.get(&type_name)?;
            return match ext {
                ExtendedType::Object(o) if o.fields.contains_key(&field_name) => {
                    let pos = ObjectFieldArgumentDefinitionPosition {
                        type_name,
                        field_name,
                        argument_name: arg_name,
                    };
                    pos.try_get(schema.schema()).is_some().then(|| {
                        ParsedSupergraphCoordinate::Target(
                            DirectiveTargetPosition::ObjectFieldArgument(pos),
                        )
                    })
                }
                ExtendedType::Interface(i) if i.fields.contains_key(&field_name) => {
                    let pos = InterfaceFieldArgumentDefinitionPosition {
                        type_name,
                        field_name,
                        argument_name: arg_name,
                    };
                    pos.try_get(schema.schema()).is_some().then(|| {
                        ParsedSupergraphCoordinate::Target(
                            DirectiveTargetPosition::InterfaceFieldArgument(pos),
                        )
                    })
                }
                _ => None,
            };
        }
    }

    if let Some(dot) = coordinate.rfind('.') {
        let left = &coordinate[..dot];
        let right = &coordinate[dot + 1..];
        let type_name = parse_name(left)?;
        let member_name = parse_name(right)?;
        return field_or_member_target(schema, type_name, member_name)
            .map(ParsedSupergraphCoordinate::Target);
    }

    let type_name = parse_name(coordinate)?;
    type_level_target(schema, type_name).map(ParsedSupergraphCoordinate::Target)
}

fn directive_target_is_inaccessible(
    pos: &DirectiveTargetPosition,
    schema: &FederationSchema,
    inaccessible_directive: &Name,
) -> Result<bool, crate::error::FederationError> {
    match pos {
        DirectiveTargetPosition::Schema(p) => Ok(p
            .get(schema.schema())
            .directives
            .has(inaccessible_directive)),
        DirectiveTargetPosition::ScalarType(p) => p.is_inaccessible(schema, inaccessible_directive),
        DirectiveTargetPosition::ObjectType(p) => p.is_inaccessible(schema, inaccessible_directive),
        DirectiveTargetPosition::ObjectField(p) => {
            p.is_inaccessible(schema, inaccessible_directive)
        }
        DirectiveTargetPosition::ObjectFieldArgument(p) => {
            p.is_inaccessible(schema, inaccessible_directive)
        }
        DirectiveTargetPosition::InterfaceType(p) => {
            p.is_inaccessible(schema, inaccessible_directive)
        }
        DirectiveTargetPosition::InterfaceField(p) => {
            p.is_inaccessible(schema, inaccessible_directive)
        }
        DirectiveTargetPosition::InterfaceFieldArgument(p) => {
            p.is_inaccessible(schema, inaccessible_directive)
        }
        DirectiveTargetPosition::UnionType(p) => p.is_inaccessible(schema, inaccessible_directive),
        DirectiveTargetPosition::EnumType(p) => p.is_inaccessible(schema, inaccessible_directive),
        DirectiveTargetPosition::EnumValue(p) => p.is_inaccessible(schema, inaccessible_directive),
        DirectiveTargetPosition::InputObjectType(p) => {
            p.is_inaccessible(schema, inaccessible_directive)
        }
        DirectiveTargetPosition::InputObjectField(p) => {
            p.is_inaccessible(schema, inaccessible_directive)
        }
        DirectiveTargetPosition::DirectiveArgument(p) => {
            p.is_inaccessible(schema, inaccessible_directive)
        }
    }
}

fn referencer_base_type_name(
    pos: &DirectiveTargetPosition,
    schema: &FederationSchema,
) -> Option<Name> {
    match pos {
        DirectiveTargetPosition::ObjectField(p) => p
            .get(schema.schema())
            .ok()
            .map(|f| f.ty.inner_named_type().clone()),
        DirectiveTargetPosition::InterfaceField(p) => p
            .get(schema.schema())
            .ok()
            .map(|f| f.ty.inner_named_type().clone()),
        DirectiveTargetPosition::ObjectFieldArgument(p) => p
            .get(schema.schema())
            .ok()
            .map(|a| a.ty.inner_named_type().clone()),
        DirectiveTargetPosition::InterfaceFieldArgument(p) => p
            .get(schema.schema())
            .ok()
            .map(|a| a.ty.inner_named_type().clone()),
        DirectiveTargetPosition::InputObjectField(p) => p
            .get(schema.schema())
            .ok()
            .map(|f| f.ty.inner_named_type().clone()),
        DirectiveTargetPosition::DirectiveArgument(p) => p
            .get(schema.schema())
            .ok()
            .map(|a| a.ty.inner_named_type().clone()),
        _ => None,
    }
}

fn referencer_matches_required_inaccessible(
    pos: &DirectiveTargetPosition,
    schema: &FederationSchema,
) -> bool {
    match pos {
        DirectiveTargetPosition::ObjectFieldArgument(p) => p
            .get(schema.schema())
            .is_ok_and(|a| a.ty.is_non_null() || a.default_value.is_none()),
        DirectiveTargetPosition::InterfaceFieldArgument(p) => p
            .get(schema.schema())
            .is_ok_and(|a| a.ty.is_non_null() || a.default_value.is_none()),
        DirectiveTargetPosition::InputObjectField(p) => p
            .get(schema.schema())
            .is_ok_and(|f| f.ty.is_non_null() || f.default_value.is_none()),
        DirectiveTargetPosition::DirectiveArgument(p) => p
            .get(schema.schema())
            .is_ok_and(|a| a.ty.is_non_null() || a.default_value.is_none()),
        _ => false,
    }
}

/// Port of JS `isRelevantSubgraphReferencer` for `@inaccessible` API schema errors.
fn is_relevant_subgraph_referencer(
    code: &ErrorCode,
    referencer: &DirectiveTargetPosition,
    supergraph: &FederationSchema,
    inaccessible_element_type_names: &[String],
    subgraph_has_inaccessible_elements: bool,
) -> bool {
    match code {
        ErrorCode::ReferencedInaccessible => {
            let Some(first) = inaccessible_element_type_names.first() else {
                return false;
            };
            let Ok(expected) = Name::new(first.as_str()) else {
                return false;
            };
            referencer_base_type_name(referencer, supergraph).is_some_and(|ty| ty == expected)
        }
        ErrorCode::DefaultValueUsesInaccessible | ErrorCode::ImplementedByInaccessible => true,
        ErrorCode::RequiredInaccessible => {
            referencer_matches_required_inaccessible(referencer, supergraph)
        }
        ErrorCode::DisallowedInaccessible | ErrorCode::OnlyInaccessibleChildren => {
            subgraph_has_inaccessible_elements
        }
        _ => false,
    }
}

/// Port of `Merger.updateInaccessibleErrorsWithLinkToSubgraphs`: map a federation error from API
/// schema validation into merge errors with subgraph source locations for `@inaccessible` issues.
///
/// In JS, subgraph hints are attached by rewriting the GraphQL error's AST nodes
/// (`withModifiedErrorNodes`). In Rust, the same information is carried on
/// [`crate::error::CompositionError::MergeError`] as [`crate::error::Locations`].
pub(crate) fn update_inaccessible_errors_with_link_to_subgraphs(
    supergraph: &FederationSchema,
    subgraphs: &[Subgraph<crate::subgraph::typestate::Validated>],
    err: crate::error::FederationError,
) -> Vec<crate::error::CompositionError> {
    err.into_errors()
        .into_iter()
        .map(|error| {
            let locations =
                subgraph_locations_for_single_inaccessible_error(supergraph, subgraphs, &error);
            crate::error::CompositionError::MergeError { error, locations }
        })
        .collect()
}

/// Subgraph source locations for one error (JS `withModifiedErrorNodes` per error).
fn subgraph_locations_for_single_inaccessible_error(
    supergraph: &FederationSchema,
    subgraphs: &[Subgraph<crate::subgraph::typestate::Validated>],
    error: &SingleFederationError,
) -> Locations {
    let (code, links) = match error {
        SingleFederationError::ReferencedInaccessible { links, .. }
        | SingleFederationError::DefaultValueUsesInaccessible { links, .. }
        | SingleFederationError::RequiredInaccessible { links, .. }
        | SingleFederationError::ImplementedByInaccessible { links, .. }
        | SingleFederationError::DisallowedInaccessible { links, .. }
        | SingleFederationError::OnlyInaccessibleChildren { links, .. }
        | SingleFederationError::QueryRootTypeInaccessible { links, .. } => (error.code(), links),
        _ => return Vec::new(),
    };
    let code = &code;

    let mut out: Vec<SubgraphLocation> = Vec::new();
    let mut subgraph_has_inaccessible: Vec<bool> = vec![false; subgraphs.len()];

    for coordinate in &links.elements {
        let Some(parsed) = parse_supergraph_coordinate(supergraph, coordinate) else {
            continue;
        };
        for (idx, subgraph) in subgraphs.iter().enumerate() {
            let Ok(Some(inaccessible_name)) = subgraph.inaccessible_directive_name() else {
                continue;
            };
            if !parsed.exists_in(subgraph.schema()) {
                continue;
            }
            let Ok(marked) = parsed.subgraph_marks_inaccessible_element(
                subgraph,
                &inaccessible_name,
                supergraph,
            ) else {
                continue;
            };
            if marked {
                subgraph_has_inaccessible[idx] = true;
                out.extend(parsed.locations(subgraph));
            }
        }
    }

    for coordinate in &links.referencers {
        let Some(parsed) = parse_supergraph_coordinate(supergraph, coordinate) else {
            continue;
        };
        if !parsed.exists_in(supergraph) {
            continue;
        }
        for (idx, subgraph) in subgraphs.iter().enumerate() {
            if !parsed.exists_in(subgraph.schema()) {
                continue;
            }
            match &parsed {
                ParsedSupergraphCoordinate::Target(target_pos) => {
                    if is_relevant_subgraph_referencer(
                        code,
                        target_pos,
                        supergraph,
                        &links.elements,
                        subgraph_has_inaccessible[idx],
                    ) {
                        out.extend(target_pos.locations(subgraph));
                    }
                }
                ParsedSupergraphCoordinate::DirectiveDefinition(dir_pos) => {
                    if is_relevant_directive_definition_referencer(
                        code,
                        subgraph_has_inaccessible[idx],
                    ) {
                        out.extend(dir_pos.locations(subgraph));
                    }
                }
            }
        }
    }

    out
}

/// `isRelevantSubgraphReferencer` for coordinates that resolve to a directive definition (`@name`).
/// JS `elementByCoordinate` can return a directive definition; those referencers are not field,
/// argument, or input-field definitions, so `REFERENCED_INACCESSIBLE` and `REQUIRED_INACCESSIBLE`
/// never apply (same as the `default` branch for unrelated element kinds in JS).
fn is_relevant_directive_definition_referencer(
    code: &ErrorCode,
    subgraph_has_inaccessible_elements: bool,
) -> bool {
    match code {
        ErrorCode::ReferencedInaccessible | ErrorCode::RequiredInaccessible => false,
        ErrorCode::DefaultValueUsesInaccessible | ErrorCode::ImplementedByInaccessible => true,
        ErrorCode::DisallowedInaccessible | ErrorCode::OnlyInaccessibleChildren => {
            subgraph_has_inaccessible_elements
        }
        _ => false,
    }
}
