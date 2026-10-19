//! Creating [`Shape`]s from [`apollo_compiler`] schema types.
//!
//! This module was `shape::graphql` until `shape` 0.9.0, which moved it into a
//! separate `apollo-shape` crate so that `shape` itself would no longer depend
//! on `apollo-compiler`. It lives here instead so that the GraphQL front end
//! shares this workspace's `apollo-compiler` dependency: upgrading the
//! compiler is a change to this repository alone, with no wrapper crate
//! release standing between the two.
//!
//! # Locations
//!
//! Every shape produced here carries [`Location`]s whose [`SourceId`] names the
//! GraphQL source file by its `apollo_compiler` [`FileId`], prefixed with
//! [`SOURCE_ID_PREFIX`] (see [`source_id`]). [`source_file`] maps such a
//! `SourceId` back to the
//! [`SourceFile`] in `schema.sources`, for example to compute line and column
//! ranges for diagnostics. It returns `None` for any other `SourceId`, which
//! is how a GraphQL location is told apart from the ones JSONSelection mints
//! for its own source text.

use std::cell::RefCell;
use std::sync::Arc;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::ast::Type;
use apollo_compiler::collections::HashMap;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::parser::FileId;
use apollo_compiler::parser::SourceFile;
use apollo_compiler::parser::SourceSpan;
use apollo_compiler::schema::ExtendedType;
use indexmap::IndexSet;
use shape::Shape;
use shape::location::Location;
use shape::location::SourceId;
use shape::name::Namespace;
use shape::name::NotFinal;

/// Prefix of every [`SourceId`] produced by [`source_id`], so that GraphQL
/// source ids can be told apart from other [`SourceId::Other`] values a
/// consumer may use for its own source texts.
pub(crate) const SOURCE_ID_PREFIX: &str = "graphql:";

/// The [`SourceId`] for [`Location`]s within the source file `file_id`: the
/// file's `apollo_compiler` id, prefixed with [`SOURCE_ID_PREFIX`].
///
/// The id and not the path, because a path identifies a file neither uniquely
/// nor always: [`Schema`] can be built from several documents parsed through
/// [`apollo_compiler::parser::Parser`] under the same or an empty path, and a
/// document actually named `built_in.graphql` would collide with the synthetic
/// file the compiler injects for built-in scalars and introspection types
/// ([`FileId::BUILT_IN`]). A [`FileId`] is unique by construction.
#[must_use]
pub(crate) fn source_id(file_id: FileId) -> SourceId {
    SourceId::new(format!("{SOURCE_ID_PREFIX}{file_id:?}"))
}

/// The [`FileId`] part of `source_id`, as it was rendered by [`source_id`], if
/// it was produced there.
///
/// Returned as text rather than a [`FileId`], which has no public constructor
/// from its integer: a caller resolves it by looking for the schema file whose
/// id renders the same way, which [`source_file`] does.
#[must_use]
fn source_file_id(source_id: &SourceId) -> Option<&str> {
    if let SourceId::Other(id) = source_id {
        id.strip_prefix(SOURCE_ID_PREFIX)
    } else {
        None
    }
}

/// The [`SourceFile`] in `schema.sources` that `source_id` names, if any.
///
/// Scans the schema's files rather than indexing them, since a [`FileId`]
/// cannot be reconstructed from its rendering. A schema has a handful of
/// source files and both callers are diagnostic paths, so the scan is not
/// worth avoiding; it is also what this did when it matched on paths.
#[must_use]
pub(crate) fn source_file<'a>(
    schema: &'a Schema,
    source_id: &SourceId,
) -> Option<&'a Arc<SourceFile>> {
    let wanted = source_file_id(source_id)?;
    schema
        .sources
        .iter()
        .find_map(|(file_id, file)| (format!("{file_id:?}") == wanted).then_some(file))
}

/// A schema, plus the [`SourceId`] of each of its files that has been asked
/// for, so that spans can be turned into [`Location`]s.
///
/// The memoizing is the point. [`source_id`] formats a fresh `graphql:<path>`
/// string on every call, and a shape carries locations on every node, so
/// computing them one span at a time allocates an `Arc<str>` per *location*
/// where one per *file* will do. Profiling `valid_large_body` found 103 such
/// strings live at peak for a single source file.
#[derive(Debug)]
struct Locator<'a> {
    schema: &'a Schema,
    source_ids: RefCell<HashMap<FileId, SourceId>>,
}

impl<'a> Locator<'a> {
    fn new(schema: &'a Schema) -> Self {
        Self {
            schema,
            source_ids: RefCell::new(HashMap::default()),
        }
    }

    /// The [`SourceId`] naming `file_id`, or `None` if it is not one of the
    /// schema's files. Cloning the returned id is an `Arc` bump.
    fn source_id(&self, file_id: FileId) -> Option<SourceId> {
        if let Some(id) = self.source_ids.borrow().get(&file_id) {
            return Some(id.clone());
        }
        if !self.schema.sources.contains_key(&file_id) {
            return None;
        }
        let id = source_id(file_id);
        self.source_ids.borrow_mut().insert(file_id, id.clone());
        Some(id)
    }

    /// The [`Location`] of `span`, or `None` if `span` refers to a file that is
    /// not among the schema's sources.
    fn location(&self, span: SourceSpan) -> Option<Location> {
        Some(
            self.source_id(span.file_id())?
                .location(span.offset()..span.end_offset()),
        )
    }

    fn locations(&self, span: Option<SourceSpan>) -> Vec<Location> {
        span.and_then(|span| self.location(span))
            .into_iter()
            .collect()
    }
}

/// Computes a [`Namespace<NotFinal>`] from a GraphQL `&Schema`, allowing for
/// mutual type references and recursive self-references.
#[must_use]
pub(crate) fn namespace_from_schema(schema: &Schema) -> Namespace<NotFinal> {
    GraphQLSchemaWalker::new(schema).compute_namespace()
}

/// The GraphQL built-in scalars, whose JSON representations the specification
/// fixes. Predefined in every namespace so that a schema which never mentions
/// one still has an entry for it.
const BUILT_IN_SCALARS: [&str; 5] = ["String", "Int", "Float", "Boolean", "ID"];

/// The shape of the scalar named `name`.
///
/// The five built-ins have representations the specification fixes, so they map
/// to the corresponding shapes, `ID` to `One<String, Int>` because the
/// specification allows either. A custom scalar carries whatever JSON its
/// service chooses and the schema does not say what, so it maps to `Unknown`:
/// the absence of a constraint rather than a constraint on nothing.
///
/// This is the only place the mapping lives. Every path through this module
/// that turns a scalar into a shape comes from here: the namespace's
/// predefined entries, and a field or argument whose type is a scalar. They
/// cannot disagree.
///
/// Note that [`super::schema_type_ref`] carries connectors' own
/// `ExtendedType` -> `Shape` conversion for output types, and its scalar arm
/// agrees with this one, `ID` included. The two are separate paths over the
/// same ground; if either moves, check the other.
fn scalar_shape(name: &str, locations: Vec<Location>) -> Shape {
    match name {
        "String" => Shape::string(locations),
        "Int" => Shape::int(locations),
        "Float" => Shape::float(locations),
        "Boolean" => Shape::bool(locations),
        "ID" => Shape::one(
            [
                Shape::string(locations.clone()),
                Shape::int(locations.clone()),
            ],
            locations,
        ),
        _ => Shape::unknown(locations),
    }
}

#[derive(Debug)]
struct GraphQLSchemaWalker<'a> {
    locator: Locator<'a>,
    visited: IndexSet<&'a str>,
}

impl<'a> GraphQLSchemaWalker<'a> {
    fn new(schema: &'a Schema) -> Self {
        Self {
            locator: Locator::new(schema),
            visited: IndexSet::new(),
        }
    }

    fn locations(&self, span: Option<SourceSpan>) -> Vec<Location> {
        self.locator.locations(span)
    }

    fn compute_namespace(mut self) -> Namespace<NotFinal> {
        let mut namespace = Namespace::new();

        // Predefine all GraphQL built-in scalars.
        for name in BUILT_IN_SCALARS {
            namespace.insert(name, scalar_shape(name, Vec::new()));
        }

        for (name, extended) in &self.locator.schema.types {
            if namespace.has(name.as_str()) {
                debug_assert!(extended.is_built_in());
            } else if !extended.is_built_in() {
                namespace.insert(name.as_str(), self.shape_from_extended_type(extended));
            }
        }

        namespace
    }

    /// The shape of a field or argument of type `ty`.
    ///
    /// Recursive over the type's own structure, one wrapper at a time, so that
    /// every level keeps its own nullability and a list of lists stays a list
    /// of lists. `[String]` is an array whose *elements* may be null, which is
    /// a different thing from `[String!]`, and `[[Int]]` is two levels deep.
    fn shape_from_type(&mut self, ty: &Type) -> Shape {
        match ty {
            Type::Named(name) => Self::nullable(self.shape_from_named_type(name)),
            Type::NonNullNamed(name) => self.shape_from_named_type(name),
            Type::List(inner) => Self::nullable(Shape::list(self.shape_from_type(inner), [])),
            Type::NonNullList(inner) => Shape::list(self.shape_from_type(inner), []),
        }
    }

    /// The shape of the named type `name`, or `Unknown` if the schema does not
    /// define it. A schema that fails validation can name a type it never
    /// declares, and a shape that admits anything is the honest answer.
    fn shape_from_named_type(&mut self, name: &Name) -> Shape {
        if let Some(extended) = self.locator.schema.types.get(name) {
            self.shape_from_extended_type(extended)
        } else {
            Shape::unknown(self.locations(name.location()))
        }
    }

    fn nullable(shape: Shape) -> Shape {
        let locations = shape.locations().cloned().collect::<Vec<_>>();
        Shape::one([shape, Shape::null([])], locations)
    }

    fn shape_from_extended_type(&mut self, extended: &'a ExtendedType) -> Shape {
        self.shape_from_extended_type_with_context(extended, false)
    }

    fn shape_from_extended_type_with_context(
        &mut self,
        extended: &'a ExtendedType,
        in_abstract_context: bool,
    ) -> Shape {
        let type_name = extended.name().as_str();

        // Check for cycles - if we're already visiting this type, return a name reference
        if self.visited.contains(type_name) {
            return Shape::name(type_name, self.locations(extended.location()));
        }

        // Mark this type as being visited
        self.visited.insert(type_name);

        let result = match extended {
            ExtendedType::Scalar(node) => scalar_shape(type_name, self.locations(node.location())),

            ExtendedType::Object(node) => {
                let node_locations = self.locations(node.location());

                // Add __typename field based on context
                let typename_shape = if in_abstract_context {
                    // In abstract context (interface/union), __typename is just the concrete type name
                    Shape::string_value(type_name, node_locations.clone())
                } else {
                    // In concrete context, __typename includes None
                    Shape::one(
                        [
                            Shape::string_value(type_name, node_locations.clone()),
                            Shape::none(),
                        ],
                        node_locations.clone(),
                    )
                };

                let fields = node
                    .fields
                    .iter()
                    .map(|(name, field)| (name.to_string(), self.shape_from_type(&field.ty)))
                    .chain(std::iter::once(("__typename".to_string(), typename_shape)))
                    .collect();

                Shape::closed_record(fields, node_locations)
            }

            ExtendedType::Interface(node) => {
                // When processing an interface, we need to create a union of implementing types
                // where each type knows it's in an abstract context for __typename generation
                let implementing_types: Vec<Shape> = self
                    .locator
                    .schema
                    .types
                    .iter()
                    .filter_map(|(_name, extended)| {
                        if let ExtendedType::Object(obj) = extended {
                            if obj.implements_interfaces.contains(&node.name) {
                                // Get the implementing type shape with abstract context
                                let impl_shape =
                                    self.shape_from_extended_type_with_context(extended, true);

                                // Assign the concrete type name to preserve field aliasing
                                let concrete_type_name = extended.name().as_str();
                                Some(impl_shape.with_base_name(
                                    concrete_type_name,
                                    self.locations(extended.location()),
                                ))
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    })
                    .collect();

                Shape::one(implementing_types, self.locations(node.location()))
            }

            ExtendedType::Union(node) => {
                let members: Vec<Shape> = node
                    .members
                    .iter()
                    .filter_map(|member| {
                        self.locator.schema.types.get(&**member).map(|extended| {
                            // Get the member type shape with abstract context
                            let member_shape =
                                self.shape_from_extended_type_with_context(extended, true);

                            // Assign the concrete type name to preserve field aliasing
                            let concrete_type_name = extended.name().as_str();
                            member_shape.with_base_name(
                                concrete_type_name,
                                self.locations(extended.location()),
                            )
                        })
                    })
                    .collect();

                Shape::one(members, self.locations(node.location()))
            }

            ExtendedType::Enum(node) => {
                // `locator` rather than `self` inside the closure, so that the
                // lazy iterator shares the borrow with the argument beside it:
                // `connectors` denies `clippy::needless_collect`, where `shape`
                // did not.
                let locator = &self.locator;
                Shape::one(
                    node.values.iter().map(|(name, _)| {
                        Shape::string_value(name.as_str(), locator.locations(name.location()))
                    }),
                    self.locations(node.location()),
                )
            }

            ExtendedType::InputObject(node) => Shape::closed_record(
                node.fields
                    .iter()
                    .map(|(name, field)| (name.to_string(), self.shape_from_type(&field.ty)))
                    .collect(),
                self.locations(node.location()),
            ),
        };

        // Remove this type from visited set now that we're done processing it
        self.visited.shift_remove(type_name);

        result
    }
}

/// Get all the shapes for a GraphQL [`Schema`], keyed by their GraphQL [`Name`].
///
/// Locations are set automatically from the schema's sources; see the module
/// documentation.
///
/// A cyclic type reference becomes a [`Shape::name`] reference rather than an
/// infinite expansion, and this function drops the [`Namespace`] those names
/// were bound in, so `shape` itself cannot resolve them. Callers resolve a
/// name by looking it up in the returned map; see `validation::expression`,
/// which does that and breaks cycles with a `resolving` set of its own.
///
/// That is verbatim what `shape::graphql::shapes_for_schema` did in 0.8.3, the
/// version this replaces, down to this line: the namespace is finalized as a
/// temporary and only its entries are kept. Nothing about the behavior is new
/// here.
#[must_use]
pub(crate) fn shapes_for_schema(schema: &Schema) -> IndexMap<String, Shape> {
    namespace_from_schema(schema).finalize().iter().collect()
}

/// The shape of a field's arguments: a closed record with one entry per
/// argument. `schema` supplies the sources that argument spans refer to.
#[must_use]
pub(crate) fn shape_for_arguments(
    schema: &Schema,
    field_definition: &Node<FieldDefinition>,
) -> Shape {
    let locator = Locator::new(schema);
    // Accumulate the locations of all the arguments so they can be highlighted together
    let mut source_span = None;
    for next_source_span in field_definition.arguments.iter().map(Node::location) {
        source_span = SourceSpan::recompose(source_span, next_source_span);
    }
    Shape::closed_record(
        field_definition
            .arguments
            .iter()
            .map(|arg| {
                (
                    arg.name.to_string(),
                    shape_from_type(&locator, arg.ty.as_ref(), locator.locations(arg.location())),
                )
            })
            .collect(),
        locator.locations(source_span),
    )
}

/// The shape of a reference to the named type `name`, for the argument path,
/// which has no [`Namespace`] to expand names into: anything but a built-in
/// scalar stays a [`Shape::name`] reference for the caller to resolve.
///
/// In `apollo-shape` this was `impl ToShape for Name`, one of a family of
/// conversions that left every other named type nominal. The rest of that
/// family is not carried over: a nominal reference is all a conversion without
/// namespace context can produce, which is not enough for abstract types,
/// where the walker above expands each member with its own `__typename`
/// literal so that a fragment can be discriminated. Connectors converts output
/// types through [`super::schema_type_ref`] instead.
fn shape_for_named_type_ref(locator: &Locator, name: &Name) -> Shape {
    let locs = locator.locations(name.location());
    let name = name.as_str();
    if BUILT_IN_SCALARS.contains(&name) {
        scalar_shape(name, locs)
    } else {
        Shape::name(name, locs)
    }
}

fn nullable(shape: Shape, locs: Vec<Location>) -> Shape {
    Shape::one([shape, Shape::null(locs.clone())], locs)
}

fn shape_from_type(locator: &Locator, ty: &Type, locs: Vec<Location>) -> Shape {
    match ty {
        Type::Named(name) => nullable(shape_for_named_type_ref(locator, name), locs),
        Type::NonNullNamed(name) => shape_for_named_type_ref(locator, name),
        Type::List(ty) => nullable(
            Shape::list(
                shape_from_type(locator, ty.as_ref(), locs.clone()),
                locs.clone(),
            ),
            locs,
        ),
        Type::NonNullList(ty) => {
            Shape::list(shape_from_type(locator, ty.as_ref(), locs.clone()), locs)
        }
    }
}

#[cfg(test)]
mod tests {
    use shape::ShapeCase;

    use super::*;

    /// A `SourceId` has to name exactly one of the schema's files, and a path
    /// cannot: `apollo_compiler` injects its built-in scalars and
    /// introspection types as a synthetic file whose path is literally
    /// `built_in.graphql`, so a document of that name gives two files the same
    /// path. `Schema` can also be built from several documents parsed under
    /// the same or an empty path.
    ///
    /// Keying on [`FileId`] rather than the path is what makes each id
    /// distinct. Before it did, both files rendered the same `SourceId` and
    /// `source_file` resolved either to whichever the scan reached first, so a
    /// diagnostic could be attributed to the wrong file, or to the compiler's
    /// own source instead of the user's.
    #[test]
    fn source_ids_survive_a_schema_file_named_like_the_built_in_one() {
        let schema =
            Schema::parse("type Query { me: String }", "built_in.graphql").expect("parse failed");

        // The premise: two files, one path between them.
        let colliding = schema
            .sources
            .iter()
            .filter(|(_, file)| file.path().to_string_lossy() == "built_in.graphql")
            .count();
        assert_eq!(
            colliding, 2,
            "expected the synthetic file and the parsed one to share a path",
        );

        // Every file's id round-trips to that same file, not to its twin.
        for (file_id, file) in schema.sources.iter() {
            let resolved = source_file(&schema, &source_id(*file_id))
                .expect("a schema file's own source id resolves");
            assert!(
                Arc::ptr_eq(resolved, file),
                "source id for {file_id:?} resolved to a different file",
            );
        }

        // And the ids really are distinct, which is what the paths were not.
        let ids: IndexSet<_> = schema.sources.keys().map(|id| source_id(*id)).collect();
        assert_eq!(ids.len(), schema.sources.len(), "source ids are unique");
    }

    /// A self-reference (`bestFriend`), a reference through a list
    /// (`friends`), and a mutual reference (`Person.pets` / `Pet.owner`).
    /// Each cycle has to become a name reference rather than an infinite
    /// expansion.
    const RECURSIVE_SDL: &str = r"
        type Query {
          me: Person
        }

        type Person {
          id: ID!
          bestFriend: Person
          friends: [Person]
          pets: [Pet!]!
        }

        type Pet {
          name: String!
          owner: Person!
        }
    ";

    /// The walker turns a cycle into a [`Shape::name`] reference instead of
    /// recursing forever: reaching this assertion at all is half the test.
    #[test]
    fn cyclic_types_become_name_references() {
        let schema = Schema::parse(RECURSIVE_SDL, "recursive.graphql").expect("parse failed");
        let shapes = shapes_for_schema(&schema);

        let person = shapes.get("Person").expect("Person has a shape");
        assert_eq!(
            person.field("bestFriend", []).pretty_print(),
            "One<Person, null>"
        );
        assert_eq!(
            person.field("friends", []).pretty_print(),
            "One<[...One<Person, null>], null>"
        );

        // The built-in scalars are predefined whether or not the schema
        // mentions them, so a schema naming only ID still has all five.
        for scalar in BUILT_IN_SCALARS {
            assert!(shapes.contains_key(scalar), "{scalar} is predefined");
        }
    }

    /// Those name references are *unbound*: `shapes_for_schema` finalizes a
    /// namespace and returns only its entries, so the `Namespace<Final>` that
    /// owned the bindings is dropped and `shape`'s own weak-scope resolution
    /// can no longer reach them. Callers resolve names by looking them up in
    /// the returned map; `validation::expression` does exactly that, with a
    /// `resolving` set to break the cycle on its side.
    ///
    /// This is inherited behavior, recorded rather than endorsed. `shape`
    /// 0.8.3's own `shapes_for_schema` had the identical body and the same
    /// weak-scope `finalize`, so the test pins what the crate this replaces
    /// already did, not something internalizing introduced. Handing back the
    /// `Namespace<Final>` instead would keep the bindings alive and let
    /// `shape` resolve them itself, at which point this test should fail and
    /// be rewritten.
    #[test]
    fn name_references_are_unbound_once_the_namespace_is_dropped() {
        let schema = Schema::parse(RECURSIVE_SDL, "recursive.graphql").expect("parse failed");
        let shapes = shapes_for_schema(&schema);

        let best_friend = shapes
            .get("Person")
            .expect("Person has a shape")
            .field("bestFriend", []);

        let ShapeCase::One(members) = best_friend.case() else {
            panic!("expected a union, got {}", best_friend.pretty_print());
        };
        let named = members
            .iter()
            .find(|member| matches!(member.case(), ShapeCase::Name(..)))
            .expect("one member is a name reference");
        let ShapeCase::Name(name, scope) = named.case() else {
            unreachable!("just matched")
        };

        assert_eq!(name.base_shape_name(), "Person");
        assert!(
            scope.upgrade(name).is_none(),
            "the finalized namespace is gone, so the name does not resolve"
        );

        // The practical consequence: an unbound name carries no structure, so
        // it is not known to be an object even though `Person` is one.
        assert!(!best_friend.is_object());
        assert!(
            shapes
                .get("Person")
                .expect("Person has a shape")
                .is_object(),
            "the top-level entry is still a record"
        );
    }
}
