//! The fixed schema and the byte-to-operation grammar shared with the Lean oracle.
//!
//! Differential fuzzing needs both implementations to receive *the same* input without a
//! serialization format in between. Following the precedent in `graphql-static-analysis-rs`, the
//! schema is a constant baked into both sides and only the operation pair varies: a byte string
//! is decoded, by the same total grammar implemented twice, into two operations.
//!
//! Everything here is mirrored in `lean/QueryInclusionOracle.lean`. Any change to the tables, the
//! grammar, or the decode order must land on both sides in the same commit, or the two will
//! silently compare different queries. [`schema_digest`] exists to catch exactly that: the oracle
//! reports its own digest at startup and the runner refuses to proceed if they differ.

use std::fmt::Write as _;

//==================================================================================================
// The fixed schema

/// The schema both sides analyze.
///
/// It is chosen to make the inclusion checker work: `I` and `K` have identical possible types
/// under different names, `J` overlaps both partially, and `Dog.friend` is a covariant override
/// so that a child region genuinely narrows below its declared interface.
pub const SCHEMA_SDL: &str = r#"
type Query {
    animals: [Animal!]!
}

interface Animal {
    name: String!
    id: String!
    tag(n: Int!, label: String, flag: Boolean, tags: [String], meta: Meta, kind: Kind): String!
    friend: Animal!
    friends: [Animal!]!
    pack: Pack!
}

interface I {
    name: String!
    id: String!
    tag(n: Int!, label: String, flag: Boolean, tags: [String], meta: Meta, kind: Kind): String!
    friend: Animal!
    friends: [Animal!]!
    pack: Pack!
}

interface J {
    name: String!
    id: String!
    tag(n: Int!, label: String, flag: Boolean, tags: [String], meta: Meta, kind: Kind): String!
    friend: Animal!
    friends: [Animal!]!
    pack: Pack!
}

interface K {
    name: String!
    id: String!
    tag(n: Int!, label: String, flag: Boolean, tags: [String], meta: Meta, kind: Kind): String!
    friend: Animal!
    friends: [Animal!]!
    pack: Pack!
}

input Meta {
    count: Int
    nested: [Int]
}

enum Kind {
    A
    B
}

type Dog implements Animal & I & J & K {
    name: String!
    id: String!
    tag(n: Int!, label: String, flag: Boolean, tags: [String], meta: Meta, kind: Kind): String!
    friend: Dog!
    friends: [Animal!]!
    pack: Pack!
    bark: String!
}

type Cat implements Animal & I & K {
    name: String!
    id: String!
    tag(n: Int!, label: String, flag: Boolean, tags: [String], meta: Meta, kind: Kind): String!
    friend: Cat!
    friends: [Animal!]!
    pack: Pack!
    purr: String!
}

type Fox implements Animal & J {
    name: String!
    id: String!
    tag(n: Int!, label: String, flag: Boolean, tags: [String], meta: Meta, kind: Kind): String!
    friend: Animal!
    friends: [Animal!]!
    pack: Pack!
}

union Pack = Dog | Cat
"#;

/// Composite type names in the order the grammar indexes them.
pub const COMPOSITE_TYPES: [&str; 8] = ["Animal", "I", "J", "K", "Dog", "Cat", "Fox", "Pack"];

/// Possible object types per composite type, aligned with [`COMPOSITE_TYPES`] and sorted by name.
pub const POSSIBLE_TYPES: [&[&str]; 8] = [
    &["Cat", "Dog", "Fox"], // Animal
    &["Cat", "Dog"],        // I
    &["Dog", "Fox"],        // J
    &["Cat", "Dog"],        // K
    &["Dog"],               // Dog
    &["Cat"],               // Cat
    &["Fox"],               // Fox
    &["Cat", "Dog"],        // Pack, the union
];

/// Selectable fields, in the order the grammar indexes them.
pub const FIELDS: [&str; 8] = [
    "name", "id", "tag", "friend", "friends", "pack", "bark", "purr",
];

/// The leading [`FIELDS`] entries that are leaves and exist on every object type. Response slot 0
/// is always drawn from these, which guarantees the fallback in `decode_selection` can always
/// find *some* selectable slot. Without that guarantee roughly 15% of documents were discarded:
/// at zero budget under `Animal` nothing else is selectable, and four slots drawing only
/// restricted or composite fields left the grammar with no legal field to emit.
const UNIVERSAL_LEAF_FIELDS: usize = 3;

/// A union has no fields of its own: GraphQL only allows `__typename` and inline fragments inside
/// one. The grammar therefore emits nothing but fragments in a union scope.
pub fn is_union(type_name: &str) -> bool {
    type_name == "Pack"
}

/// Is `field` declared on `parent_type`? `bark` and `purr` exist on one object type each, so that
/// a response name bound to either is only selectable in part of the schema.
pub fn field_defined_on(parent_type: &str, field: &str) -> bool {
    match field {
        "bark" => parent_type == "Dog",
        "purr" => parent_type == "Cat",
        _ => !is_union(parent_type),
    }
}

/// Does this field take arguments?
pub fn field_takes_argument(field: &str) -> bool {
    field == "tag"
}

/// The static type of a field's selection set, or `None` for a leaf.
///
/// `Dog.friend` is the covariant override, so the parent type is what decides.
pub fn field_output_type(parent_type: &str, field: &str) -> Option<&'static str> {
    match field {
        // Two covariant overrides, so a narrowed scope can reach two different child regions.
        "friend" => Some(match parent_type {
            "Dog" => "Dog",
            "Cat" => "Cat",
            _ => "Animal",
        }),
        "friends" => Some("Animal"),
        "pack" => Some("Pack"),
        _ => None,
    }
}

fn possible_types_of(type_name: &str) -> &'static [&'static str] {
    let index = COMPOSITE_TYPES
        .iter()
        .position(|candidate| *candidate == type_name)
        .expect("composite type");
    POSSIBLE_TYPES[index]
}

/// The type conditions that can legally appear inside a selection set on `parent_type`.
///
/// GraphQL rejects a fragment whose type cannot overlap its parent, and an invalid document would
/// be thrown away instead of tested, so only overlapping conditions are offered. Union conditions
/// are excluded as well: narrowing a union scope to another union would leave the grammar still
/// unable to emit a field, and it could run out of budget that way.
pub fn valid_type_conditions(parent_type: &str) -> Vec<&'static str> {
    let parent_possible = possible_types_of(parent_type);
    COMPOSITE_TYPES
        .iter()
        .copied()
        .filter(|candidate| !is_union(candidate))
        .filter(|candidate| {
            possible_types_of(candidate)
                .iter()
                .any(|ty| parent_possible.contains(ty))
        })
        .collect()
}

/// A canonical rendering of everything above, so the oracle can prove it holds the same schema.
pub fn schema_digest() -> String {
    let mut digest = String::new();
    for (index, type_name) in COMPOSITE_TYPES.iter().enumerate() {
        let _ = write!(
            digest,
            "{type_name}:{}:{};",
            POSSIBLE_TYPES[index].join(","),
            is_union(type_name) as u8
        );
    }
    for field in FIELDS {
        let _ = write!(digest, "{field}/{}/", field_takes_argument(field) as u8);
        for parent in ["Animal", "Dog", "Cat", "Fox"] {
            let _ = write!(
                digest,
                "{}{}",
                field_defined_on(parent, field) as u8,
                field_output_type(parent, field).unwrap_or("-"),
            );
        }
        let _ = write!(digest, ";");
    }
    for variant in 0..VARIABLE_DECLARATIONS {
        let _ = write!(digest, "{};", render_variable_declaration("v", variant));
    }
    for variant in 0..INT_DECLARATIONS {
        let _ = write!(digest, "{};", render_int_declaration("i", variant));
    }
    for variant in 0..STRING_DECLARATIONS {
        let _ = write!(digest, "{};", render_string_declaration("s", variant));
    }
    digest.push_str(&argument_digest());
    digest
}

//==================================================================================================
// The byte grammar

/// How many times the grammar may nest below `animals`. Every recursive step spends one unit,
/// whether it opens a response boundary or an inline fragment, so the decoder is structurally
/// recursive on it. That matters because the Lean side has to prove termination, and a budget
/// that only some branches decrease would need a lexicographic measure there for no benefit here.
const NESTING_BUDGET: usize = 5;
/// Response names available to one operation. Each is bound to one resolver call for the whole
/// operation.
const RESPONSE_SLOTS: usize = 4;
/// Directive combinations a selection can carry.
const DIRECTIVE_CHOICES: usize = 12;
/// Boolean variables, referenced by `@skip`/`@include`.
pub const VARIABLES: [&str; 3] = ["v0", "v1", "v2"];
/// `Int!` variables, referenced by the required `n` argument.
pub const INT_VARIABLES: [&str; 2] = ["i0", "i1"];
/// `String` variables, referenced by the optional `label` argument.
pub const STRING_VARIABLES: [&str; 1] = ["s0"];
/// Ways one `Int` variable can be declared. A nullable variable needs a non-null default to be
/// usable at a non-null location, so there is no bare `Int`.
pub const INT_DECLARATIONS: usize = 3;
/// Ways one `String` variable can be declared. `label` is nullable, so a bare `String` is fine.
pub const STRING_DECLARATIONS: usize = 2;

pub fn render_int_declaration(name: &str, variant: usize) -> String {
    match variant % INT_DECLARATIONS {
        0 => format!("${name}: Int!"),
        1 => format!("${name}: Int! = 1"),
        _ => format!("${name}: Int = 1"),
    }
}

pub fn render_string_declaration(name: &str, variant: usize) -> String {
    match variant % STRING_DECLARATIONS {
        0 => format!("${name}: String"),
        _ => format!("${name}: String = \"a\""),
    }
}
/// Ways one variable can be declared. Every variant is usable in `if:` position: a nullable
/// variable needs a non-null default to be, which is why there is no bare `Boolean`.
pub const VARIABLE_DECLARATIONS: usize = 5;

/// Renders one variable declaration.
///
/// Varying these is the only way to reach the shared-declaration compatibility check, which the
/// Lean model states as `sharedVariableDefinitionsSyntacticallyCompatible` and which is otherwise
/// vacuously true when both operations declare everything identically.
pub fn render_variable_declaration(name: &str, variant: usize) -> String {
    match variant % VARIABLE_DECLARATIONS {
        0 => format!("${name}: Boolean!"),
        1 => format!("${name}: Boolean! = true"),
        2 => format!("${name}: Boolean! = false"),
        3 => format!("${name}: Boolean = true"),
        _ => format!("${name}: Boolean = false"),
    }
}

/// A cursor that never fails: past the end every byte reads as zero, so decoding is total for
/// every input libFuzzer can produce.
struct Cursor<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl Cursor<'_> {
    fn next(&mut self) -> usize {
        let byte = self.bytes.get(self.position).copied().unwrap_or(0);
        self.position += 1;
        byte as usize
    }
}

/// What one response name resolves to for the whole of one operation.
///
/// Binding the field and its arguments per response name is what keeps generated documents valid:
/// GraphQL rejects an operation that selects one response name as two different resolver calls.
/// Left and right get *independent* tables, so a field-name or argument difference between the
/// two operations is still reachable — which is the case the checker has to detect.
/// The argument set one response name passes.
///
/// Chosen to reach every arm the checker's value comparison distinguishes. `tags` is a list, whose
/// items compare order-**sensitively**; `meta` is an input object, whose fields compare
/// order-**insensitively**. Neither arm could execute at all while the only arguments were an
/// `Int` and a `String`, which made them the largest cluster of surviving mutants.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Arguments {
    /// `n: Int!`, the one required argument. Passed as a literal unless `n_variable` is set.
    n: i32,
    /// When set, `n` is passed as this variable instead. `tag(n: $i0)`, `tag(n: $i1)` and
    /// `tag(n: 1)` are three different resolver calls, which is what makes this worth generating.
    n_variable: Option<&'static str>,
    label: Option<&'static str>,
    /// When set, and `label` is present, `label` is passed as this variable.
    label_variable: Option<&'static str>,
    flag: Option<bool>,
    /// `[String]`. Choice 3 is choice 2 reversed, which must compare *unequal* to it.
    tags: Option<&'static [&'static str]>,
    /// `Meta`. Choice 3 is choice 2 with its fields reversed, which must compare *equal* to it.
    meta: Option<&'static str>,
    kind: Option<&'static str>,
    /// Emit the arguments in reverse. Argument order is not semantically meaningful, so both
    /// sides must agree that a permutation changes nothing.
    swapped: bool,
}

const TAG_LISTS: [&[&str]; 3] = [&["a"], &["a", "b"], &["b", "a"]];
const META_OBJECTS: [&str; 3] = [
    "{count: 1}",
    "{count: 1, nested: [1]}",
    "{nested: [1], count: 1}",
];

#[derive(Debug, Clone, PartialEq, Eq)]
struct Slot {
    field: &'static str,
    arguments: Arguments,
}

#[derive(Debug, Clone)]
pub(crate) enum Node {
    Field {
        slot: usize,
        directives: usize,
        children: Vec<Node>,
    },
    Fragment {
        type_condition: Option<&'static str>,
        directives: usize,
        children: Vec<Node>,
    },
}

/// Which declaration variant each variable of the operation uses.
#[derive(Debug, Clone, Default)]
pub(crate) struct Declarations {
    booleans: Vec<usize>,
    ints: Vec<usize>,
    strings: Vec<usize>,
}

impl Declarations {
    /// Every variable declared identically. Used where the declaration dimension would otherwise
    /// confound the property under test.
    fn uniform() -> Declarations {
        Declarations {
            booleans: vec![0; VARIABLES.len()],
            ints: vec![0; INT_VARIABLES.len()],
            strings: vec![0; STRING_VARIABLES.len()],
        }
    }
}

/// Which variables an operation's rendering actually referenced. GraphQL rejects an operation that
/// declares a variable it never uses, so only these are declared.
#[derive(Debug, Clone, Default)]
struct Used {
    booleans: [bool; VARIABLES.len()],
    ints: [bool; INT_VARIABLES.len()],
    strings: [bool; STRING_VARIABLES.len()],
}

#[derive(Debug, Clone)]
pub(crate) struct DecodedOperation {
    slots: Vec<Slot>,
    declarations: Declarations,
    selections: Vec<Node>,
}

/// Two bytes of argument choices. Kept in one place because the Lean adapter mirrors it exactly.
fn decode_arguments(first: usize, second: usize) -> Arguments {
    Arguments {
        n: (first % 2) as i32,
        n_variable: match (first / 18) % 3 {
            0 => None,
            other => Some(INT_VARIABLES[other - 1]),
        },
        label: match (first / 2) % 3 {
            0 => None,
            1 => Some("a"),
            _ => Some("b"),
        },
        label_variable: match (second / 96) % 2 {
            0 => None,
            _ => Some(STRING_VARIABLES[0]),
        },
        flag: match (first / 6) % 3 {
            0 => None,
            1 => Some(true),
            _ => Some(false),
        },
        tags: match second % 4 {
            0 => None,
            other => Some(TAG_LISTS[other - 1]),
        },
        meta: match (second / 4) % 4 {
            0 => None,
            other => Some(META_OBJECTS[other - 1]),
        },
        kind: match (second / 16) % 3 {
            0 => None,
            1 => Some("A"),
            _ => Some("B"),
        },
        swapped: (second / 48) % 2 == 1,
    }
}

fn decode_slots(cursor: &mut Cursor<'_>) -> Vec<Slot> {
    (0..RESPONSE_SLOTS)
        .map(|index| {
            let choices = if index == 0 {
                UNIVERSAL_LEAF_FIELDS
            } else {
                FIELDS.len()
            };
            let field = FIELDS[cursor.next() % choices];
            let first = cursor.next();
            let second = cursor.next();
            Slot {
                field,
                arguments: decode_arguments(first, second),
            }
        })
        .collect()
}

/// Can this slot be selected on `parent_type` with `budget` left?
///
/// A field must be declared there, a composite field needs room for its selection set, and a
/// union-returning field needs two units: one for its own selection set and one for the inline
/// fragment that set is required to consist of.
fn slot_is_selectable(slot: &Slot, parent_type: &str, budget: usize) -> bool {
    if !field_defined_on(parent_type, slot.field) {
        return false;
    }
    match field_output_type(parent_type, slot.field) {
        None => true,
        Some(child) if is_union(child) => budget >= 2,
        Some(_) => budget >= 1,
    }
}

fn decode_selection_set(
    cursor: &mut Cursor<'_>,
    slots: &[Slot],
    parent_type: &str,
    budget: usize,
) -> Vec<Node> {
    let count = cursor.next() % 3 + 1;
    (0..count)
        .map(|_| decode_selection(cursor, slots, parent_type, budget))
        .collect()
}

fn decode_selection(
    cursor: &mut Cursor<'_>,
    slots: &[Slot],
    parent_type: &str,
    budget: usize,
) -> Node {
    let byte = cursor.next();
    let conditions = valid_type_conditions(parent_type);

    // Inside a union only fragments are legal, and they must narrow to a concrete type — the
    // budget rule in `slot_is_selectable` guarantees there is room for one, so the `budget > 0`
    // guard is unreachable. It is written this way so the Lean adapter can match on the budget
    // first and stay structurally recursive.
    if is_union(parent_type) && budget > 0 {
        let type_condition = conditions[cursor.next() % conditions.len()];
        let directives = cursor.next() % DIRECTIVE_CHOICES;
        let children =
            decode_selection_set(cursor, slots, type_condition, budget.saturating_sub(1));
        return Node::Fragment {
            type_condition: Some(type_condition),
            directives,
            children,
        };
    }

    if budget > 0 && byte % 4 == 3 {
        let choice = cursor.next() % (conditions.len() + 1);
        let type_condition = (choice > 0).then(|| conditions[choice - 1]);
        let directives = cursor.next() % DIRECTIVE_CHOICES;
        let children = decode_selection_set(
            cursor,
            slots,
            type_condition.unwrap_or(parent_type),
            budget - 1,
        );
        return Node::Fragment {
            type_condition,
            directives,
            children,
        };
    }

    let mut slot = cursor.next() % RESPONSE_SLOTS;
    let directives = cursor.next() % DIRECTIVE_CHOICES;
    // A slot the budget or the parent type cannot accommodate falls back to the first one that
    // works. When the table holds none the document is left invalid on purpose and discarded;
    // Lean decodes the same tree and is never asked about a case this side discarded.
    if !slot_is_selectable(&slots[slot], parent_type, budget) {
        slot = slots
            .iter()
            .position(|candidate| slot_is_selectable(candidate, parent_type, budget))
            .unwrap_or(slot);
    }
    let children = match field_output_type(parent_type, slots[slot].field) {
        Some(child_type) if budget > 0 => {
            decode_selection_set(cursor, slots, child_type, budget - 1)
        }
        _ => Vec::new(),
    };
    Node::Field {
        slot,
        directives,
        children,
    }
}

fn decode_operation(cursor: &mut Cursor<'_>) -> DecodedOperation {
    let slots = decode_slots(cursor);
    // One byte carries the three Boolean variants (5^3 = 125); a second carries the two Int and
    // one String variants (3^2 * 2 = 18).
    let packed_booleans = cursor.next();
    let packed_typed = cursor.next();
    let declarations = Declarations {
        booleans: (0..VARIABLES.len())
            .map(|index| {
                (packed_booleans / VARIABLE_DECLARATIONS.pow(index as u32)) % VARIABLE_DECLARATIONS
            })
            .collect(),
        ints: (0..INT_VARIABLES.len())
            .map(|index| (packed_typed / INT_DECLARATIONS.pow(index as u32)) % INT_DECLARATIONS)
            .collect(),
        strings: vec![(packed_typed / 9) % STRING_DECLARATIONS],
    };
    let selections = decode_selection_set(cursor, &slots, "Animal", NESTING_BUDGET);
    DecodedOperation {
        slots,
        declarations,
        selections,
    }
}

//==================================================================================================
// Rendering

fn render_directives(choice: usize, used: &mut Used) -> String {
    let used = &mut used.booleans;
    let mut mark = |index: usize, text: &str| {
        used[index] = true;
        text.to_string()
    };
    match choice {
        1 => mark(0, " @include(if: $v0)"),
        2 => mark(0, " @skip(if: $v0)"),
        3 => mark(1, " @include(if: $v1)"),
        4 => mark(1, " @skip(if: $v1)"),
        5 => mark(2, " @include(if: $v2)"),
        6 => mark(2, " @skip(if: $v2)"),
        7 => " @include(if: true)".to_string(),
        8 => " @skip(if: false)".to_string(),
        9 => {
            used[0] = true;
            used[1] = true;
            " @include(if: $v0) @skip(if: $v1)".to_string()
        }
        10 => {
            used[1] = true;
            used[2] = true;
            " @include(if: $v1) @skip(if: $v2)".to_string()
        }
        11 => {
            used[0] = true;
            used[2] = true;
            " @skip(if: $v0) @include(if: $v2)".to_string()
        }
        _ => String::new(),
    }
}

/// The argument list as it appears in a query, or the empty string for a field that takes none.
fn render_arguments(slot: &Slot, used: &mut Used) -> String {
    if !field_takes_argument(slot.field) {
        return String::new();
    }
    if let Some(variable) = slot.arguments.n_variable {
        if let Some(index) = INT_VARIABLES.iter().position(|name| *name == variable) {
            used.ints[index] = true;
        }
    }
    if slot.arguments.label.is_some() {
        if let Some(variable) = slot.arguments.label_variable {
            if let Some(index) = STRING_VARIABLES.iter().position(|name| *name == variable) {
                used.strings[index] = true;
            }
        }
    }
    let arguments = &slot.arguments;
    let mut parts = vec![match arguments.n_variable {
        Some(variable) => format!("n: ${variable}"),
        None => format!("n: {}", arguments.n),
    }];
    if let Some(label) = arguments.label {
        parts.push(match arguments.label_variable {
            Some(variable) => format!("label: ${variable}"),
            None => format!("label: \"{label}\""),
        });
    }
    if let Some(flag) = arguments.flag {
        parts.push(format!("flag: {flag}"));
    }
    if let Some(tags) = arguments.tags {
        let items: Vec<String> = tags.iter().map(|tag| format!("\"{tag}\"")).collect();
        parts.push(format!("tags: [{}]", items.join(", ")));
    }
    if let Some(meta) = arguments.meta {
        parts.push(format!("meta: {meta}"));
    }
    if let Some(kind) = arguments.kind {
        parts.push(format!("kind: {kind}"));
    }
    if arguments.swapped {
        parts.reverse();
    }
    format!("({})", parts.join(", "))
}

/// A canonical description of one decoded argument set, for the oracle's drift check. The Lean
/// adapter builds ADTs rather than text, so the digest compares a shared description instead.
fn describe_arguments(arguments: &Arguments) -> String {
    let optional = |value: Option<String>| value.unwrap_or_else(|| "-".to_string());
    format!(
        "n{}{}|l{}{}|f{}|t{}|m{}|k{}|s{}",
        arguments.n,
        optional(arguments.n_variable.map(str::to_string)),
        optional(arguments.label.map(str::to_string)),
        optional(arguments.label_variable.map(str::to_string)),
        optional(arguments.flag.map(|flag| flag.to_string())),
        optional(arguments.tags.map(|tags| tags.join("+"))),
        optional(
            arguments
                .meta
                .map(|meta| meta.replace(' ', "").replace(['{', '}'], ""))
        ),
        optional(arguments.kind.map(str::to_string)),
        arguments.swapped as u8,
    )
}

/// Every argument encoding the grammar can produce, described canonically. Part of the schema
/// digest because the argument encoding is the most drift-prone half of the shared grammar.
pub fn argument_digest() -> String {
    let mut digest = String::new();
    for index in 0..24 {
        let arguments = decode_arguments(index * 7, index * 11);
        let _ = write!(digest, "{};", describe_arguments(&arguments));
    }
    digest
}

fn render_nodes(nodes: &[Node], slots: &[Slot], used: &mut Used, indent: usize, out: &mut String) {
    let pad = "  ".repeat(indent);
    for node in nodes {
        match node {
            Node::Field {
                slot,
                directives,
                children,
            } => {
                let entry = &slots[*slot];
                let arguments = render_arguments(entry, used);
                let directives = render_directives(*directives, used);
                let _ = write!(out, "{pad}r{slot}: {}{arguments}{directives}", entry.field);
                if children.is_empty() {
                    out.push('\n');
                } else {
                    out.push_str(" {\n");
                    render_nodes(children, slots, used, indent + 1, out);
                    let _ = writeln!(out, "{pad}}}");
                }
            }
            Node::Fragment {
                type_condition,
                directives,
                children,
            } => {
                let condition = match type_condition {
                    Some(name) => format!(" on {name}"),
                    None => String::new(),
                };
                let directives = render_directives(*directives, used);
                let _ = writeln!(out, "{pad}...{condition}{directives} {{");
                render_nodes(children, slots, used, indent + 1, out);
                let _ = writeln!(out, "{pad}}}");
            }
        }
    }
}

fn render_operation(operation: &DecodedOperation) -> String {
    let mut body = String::new();
    let mut used = Used::default();
    render_nodes(
        &operation.selections,
        &operation.slots,
        &mut used,
        2,
        &mut body,
    );

    // GraphQL rejects an operation that declares a variable it never uses, so only the reached
    // ones are declared. The Lean adapter derives the same set from the decoded selection tree.
    let mut declared: Vec<String> = VARIABLES
        .iter()
        .enumerate()
        .filter(|(index, _)| used.booleans[*index])
        .map(|(index, name)| {
            render_variable_declaration(name, operation.declarations.booleans[index])
        })
        .collect();
    declared.extend(
        INT_VARIABLES
            .iter()
            .enumerate()
            .filter(|(index, _)| used.ints[*index])
            .map(|(index, name)| render_int_declaration(name, operation.declarations.ints[index])),
    );
    declared.extend(
        STRING_VARIABLES
            .iter()
            .enumerate()
            .filter(|(index, _)| used.strings[*index])
            .map(|(index, name)| {
                render_string_declaration(name, operation.declarations.strings[index])
            }),
    );
    let header = if declared.is_empty() {
        "query".to_string()
    } else {
        format!("query({})", declared.join(", "))
    };
    format!("{header} {{\n  animals {{\n{body}  }}\n}}\n")
}

/// One decoded test case: two operation sources over [`SCHEMA_SDL`].
#[derive(Debug, Clone)]
pub struct Case {
    pub left: String,
    pub right: String,
}

/// Decodes a byte string into a case. Total: every input yields a case, though not every case is
/// a valid GraphQL document — see `Harness::prepare`.
pub fn decode(bytes: &[u8]) -> Case {
    let mut cursor = Cursor { bytes, position: 0 };
    let left = decode_operation(&mut cursor);
    let right = decode_operation(&mut cursor);
    Case {
        left: render_operation(&left),
        right: render_operation(&right),
    }
}

/// How many bytes the left operation consumes.
///
/// Purely an input-generation aid: knowing the split lets a campaign build byte strings whose two
/// halves decode to the *same* operation, and then perturb one byte to land just off it. Random
/// pairs are almost never related, and the cases that separate a correct checker from a subtly
/// wrong one sit right at the boundary between included and not included.
pub fn left_consumed(bytes: &[u8]) -> usize {
    let mut cursor = Cursor { bytes, position: 0 };
    let _ = decode_operation(&mut cursor);
    cursor.position.min(bytes.len())
}

//==================================================================================================
// Semantics-preserving rewrites
//
// The byte grammar is a closed generator: it produces the shapes someone thought to encode. A
// prototype of this work in apollo-rs saturated its coverage-guided campaign twice while three
// Rust-vs-Lean divergences and two apollo-federation bugs went unfound, because every one of them
// lived in a shape nobody had enumerated. Its conclusion was to add an *open* generator: take a
// case, then apply random sequences of edits whose effect on the verdict is known.
//
// The edits below are semantics preserving: each leaves the pair of response shapes unchanged, so
// `includes` must return exactly what it returned before. No oracle is needed to check that, which
// means this lane runs at full Rust speed and can compose edits into shapes no family enumerates.
// Two of them reproduce known bug shapes by construction: splitting a field into complementary
// `@skip`/`@include` branches emits the Boolean-union case-split shape behind router#10024, and
// partitioning a selection over its parent's runtime types emits the covariant type-case shape.

/// One edit. Each is a no-op on the response shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rewrite {
    /// Wrap in `... @include(if: true)`.
    WrapAlwaysInclude,
    /// Wrap in `... @skip(if: false)`.
    WrapNeverSkip,
    /// Wrap in `... on <parent>`, the parent's own type condition.
    WrapIdentityTypeCondition,
    /// Duplicate the selection. GraphQL merges by response name, and the grammar binds one
    /// resolver call per response name, so a second identical occurrence adds nothing.
    Duplicate,
    /// Replace the selection with complementary `@include(if: $v)` and `@skip(if: $v)` copies.
    /// Their conditions are disjoint and jointly total, so the pair is equivalent to the
    /// original whatever `$v` is -- including a `$v` the selection already depends on.
    SplitComplementary,
    /// Replace the selection with one copy per runtime type of the parent, each under its own
    /// type condition. The conditions partition the parent's possible types exactly.
    PartitionByRuntimeType,
    /// Reverse sibling order. Inclusion is decided per response name, and within one operation a
    /// response name always denotes the same resolver call, so order cannot matter.
    ReverseSiblings,
    /// Permute the fields of an input-object argument. GraphQL input objects are unordered, so
    /// this changes no resolver call.
    ///
    /// Random generation practically never produces two operations differing *only* in
    /// input-object field order, which is the one shape that can tell an order-insensitive
    /// comparison from an order-sensitive one. Generated by construction here instead.
    PermuteObjectArgument,
}

const REWRITES: [Rewrite; 8] = [
    Rewrite::WrapAlwaysInclude,
    Rewrite::WrapNeverSkip,
    Rewrite::WrapIdentityTypeCondition,
    Rewrite::Duplicate,
    Rewrite::SplitComplementary,
    Rewrite::PartitionByRuntimeType,
    Rewrite::ReverseSiblings,
    Rewrite::PermuteObjectArgument,
];

/// Directive choices used by the rewrites, by index into `render_directives`.
const DIRECTIVE_NONE: usize = 0;
const DIRECTIVE_ALWAYS_INCLUDE: usize = 7;
const DIRECTIVE_NEVER_SKIP: usize = 8;
/// `@include(if: $v0)` and `@skip(if: $v0)`, a complementary pair over one variable.
const DIRECTIVE_INCLUDE_V0: usize = 1;
const DIRECTIVE_SKIP_V0: usize = 2;

/// Does this subtree select any field that only part of the schema declares? Partitioning by
/// runtime type would push such a field under a type condition that does not declare it.
fn uses_restricted_field(node: &Node, slots: &[Slot]) -> bool {
    match node {
        Node::Field { slot, children, .. } => {
            matches!(slots[*slot].field, "bark" | "purr")
                || children
                    .iter()
                    .any(|child| uses_restricted_field(child, slots))
        }
        Node::Fragment { children, .. } => children
            .iter()
            .any(|child| uses_restricted_field(child, slots)),
    }
}

/// Does this selection contain a type condition anywhere, at any depth?
///
/// Partitioning wraps a selection in each of its parent's runtime types, which is only sound if
/// nothing inside it depends on the parent staying wide. Two ways it can:
///
/// - a fragment already narrowed to `Dog` cannot sit inside `... on Fox`; GraphQL rejects a type
///   condition that cannot overlap its parent;
/// - `Dog.friend` covariantly returns `Dog`, so narrowing an *outer* scope to `Dog` changes the
///   output type of a `friend` further in, and a condition below that field which was valid
///   against `Animal` may not be valid against `Dog`.
///
/// The second case reaches below a field boundary, so this descends through fields too. That is
/// conservative — it also skips subtrees that would have been fine — but partitioning plain
/// selections is what makes the covariant shape, and those are unaffected.
fn contains_type_condition(node: &Node) -> bool {
    match node {
        Node::Field { children, .. } => children.iter().any(contains_type_condition),
        Node::Fragment {
            type_condition,
            children,
            ..
        } => type_condition.is_some() || children.iter().any(contains_type_condition),
    }
}

fn wrap(node: Node, type_condition: Option<&'static str>, directives: usize) -> Node {
    Node::Fragment {
        type_condition,
        directives,
        children: vec![node],
    }
}

/// Applies `rewrite` to one selection, or returns it untouched when the edit does not apply here.
fn apply_rewrite(
    node: Node,
    rewrite: Rewrite,
    parent_type: &'static str,
    slots: &[Slot],
) -> Vec<Node> {
    match rewrite {
        Rewrite::WrapAlwaysInclude => vec![wrap(node, None, DIRECTIVE_ALWAYS_INCLUDE)],
        Rewrite::WrapNeverSkip => vec![wrap(node, None, DIRECTIVE_NEVER_SKIP)],
        Rewrite::WrapIdentityTypeCondition => {
            // A union parent cannot host a bare field, but the wrapper is a fragment either way.
            vec![wrap(node, Some(parent_type), DIRECTIVE_NONE)]
        }
        Rewrite::Duplicate => vec![node.clone(), node],
        Rewrite::SplitComplementary => vec![
            wrap(node.clone(), None, DIRECTIVE_INCLUDE_V0),
            wrap(node, None, DIRECTIVE_SKIP_V0),
        ],
        Rewrite::PartitionByRuntimeType => {
            if uses_restricted_field(&node, slots)
                || is_union(parent_type)
                || contains_type_condition(&node)
            {
                return vec![node];
            }
            let runtime_types = possible_types_of(parent_type);
            if runtime_types.len() < 2 {
                return vec![node];
            }
            runtime_types
                .iter()
                .map(|ty| wrap(node.clone(), Some(ty), DIRECTIVE_NONE))
                .collect()
        }
        Rewrite::ReverseSiblings | Rewrite::PermuteObjectArgument => vec![node],
    }
}

/// Where in a walk the edit lands: skip `remaining` selections, then apply once.
struct Site {
    remaining: usize,
    fired: bool,
}

/// Walks the tree, applying `rewrite` at exactly one selection.
fn rewrite_nodes(
    nodes: Vec<Node>,
    rewrite: Rewrite,
    parent_type: &'static str,
    slots: &[Slot],
    site: &mut Site,
) -> Vec<Node> {
    let mut out: Vec<Node> = Vec::new();
    for node in nodes {
        let here = if site.fired {
            false
        } else if site.remaining == 0 {
            site.fired = true;
            true
        } else {
            site.remaining -= 1;
            false
        };

        let node = match node {
            Node::Field {
                slot,
                directives,
                children,
            } => {
                let child_type = field_output_type(parent_type, slots[slot].field);
                let children = match child_type {
                    Some(child_type) if !children.is_empty() => {
                        rewrite_nodes(children, rewrite, child_type, slots, site)
                    }
                    _ => children,
                };
                Node::Field {
                    slot,
                    directives,
                    children,
                }
            }
            Node::Fragment {
                type_condition,
                directives,
                children,
            } => {
                let inner_type = type_condition.unwrap_or(parent_type);
                let children = rewrite_nodes(children, rewrite, inner_type, slots, site);
                Node::Fragment {
                    type_condition,
                    directives,
                    children,
                }
            }
        };

        if here {
            out.extend(apply_rewrite(node, rewrite, parent_type, slots));
        } else {
            out.push(node);
        }
    }
    out
}

fn rewrite_operation(
    mut operation: DecodedOperation,
    rewrite: Rewrite,
    target: usize,
) -> DecodedOperation {
    let mut site = Site {
        remaining: target,
        fired: false,
    };
    operation.selections = rewrite_nodes(
        operation.selections,
        rewrite,
        "Animal",
        &operation.slots,
        &mut site,
    );
    if rewrite == Rewrite::ReverseSiblings {
        operation.selections.reverse();
    }
    if rewrite == Rewrite::PermuteObjectArgument {
        for slot in &mut operation.slots {
            slot.arguments.meta = match slot.arguments.meta {
                Some(meta) if meta == META_OBJECTS[1] => Some(META_OBJECTS[2]),
                Some(meta) if meta == META_OBJECTS[2] => Some(META_OBJECTS[1]),
                other => other,
            };
        }
    }
    operation
}

/// A base case and the same case after semantics-preserving edits. `includes` must answer
/// identically for both, in both directions.
pub struct RewrittenCase {
    pub base: Case,
    pub rewritten: Case,
    pub applied: Vec<&'static str>,
}

/// Decodes `bytes`, then applies up to `steps` edits driven by `seed`.
pub fn rewritten_case(bytes: &[u8], seed: u64, steps: usize) -> RewrittenCase {
    let mut cursor = Cursor { bytes, position: 0 };
    let mut base_left = decode_operation(&mut cursor);
    let mut base_right = decode_operation(&mut cursor);
    // Hold variable declarations constant across both operations. `SplitComplementary` introduces
    // a variable into a side that may not have used one, which changes that side's *declared* set
    // -- and `includes` rejects when a shared declaration differs, so the verdict would change for
    // a reason that has nothing to do with the selection shapes this lane is testing. The
    // declaration dimension is exercised by the differential lane instead.
    base_left.declarations = Declarations::uniform();
    base_right.declarations = Declarations::uniform();
    let base = Case {
        left: render_operation(&base_left),
        right: render_operation(&base_right),
    };

    let mut state = seed | 1;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };

    let (mut left, mut right) = (base_left, base_right);
    let mut applied = Vec::new();
    for _ in 0..steps {
        let rewrite = REWRITES[(next() % REWRITES.len() as u64) as usize];
        let target = (next() % 6) as usize;
        if next() % 2 == 0 {
            left = rewrite_operation(left, rewrite, target);
        } else {
            right = rewrite_operation(right, rewrite, target);
        }
        applied.push(rewrite_name(rewrite));
    }

    RewrittenCase {
        base,
        rewritten: Case {
            left: render_operation(&left),
            right: render_operation(&right),
        },
        applied,
    }
}

pub fn rewrite_name(rewrite: Rewrite) -> &'static str {
    match rewrite {
        Rewrite::WrapAlwaysInclude => "wrap-always-include",
        Rewrite::WrapNeverSkip => "wrap-never-skip",
        Rewrite::WrapIdentityTypeCondition => "wrap-identity-type-condition",
        Rewrite::Duplicate => "duplicate",
        Rewrite::SplitComplementary => "split-complementary",
        Rewrite::PartitionByRuntimeType => "partition-by-runtime-type",
        Rewrite::ReverseSiblings => "reverse-siblings",
        Rewrite::PermuteObjectArgument => "permute-object-argument",
    }
}

//==================================================================================================
// Directional edits and triples
//
// The rewrites above preserve meaning, so they test that the verdict does not *change*. A checker
// that is consistently wrong the same way before and after is invisible to them. These two
// properties are independent of that one and need no oracle either.

/// Adds a selection to a scope. Response slot 0 is a universal leaf, so `r0` is selectable
/// anywhere a field is, and adding it can only add a response field — never change an existing
/// one, since the slot table binds one resolver call per response name.
fn grow_nodes(
    nodes: Vec<Node>,
    parent_type: &'static str,
    slots: &[Slot],
    site: &mut Site,
) -> Vec<Node> {
    let mut out: Vec<Node> = Vec::new();
    for node in nodes {
        let here = if site.fired {
            false
        } else if site.remaining == 0 {
            site.fired = true;
            true
        } else {
            site.remaining -= 1;
            false
        };
        let node = match node {
            Node::Field {
                slot,
                directives,
                children,
            } => {
                let children = match field_output_type(parent_type, slots[slot].field) {
                    Some(child_type) if !children.is_empty() => {
                        grow_nodes(children, child_type, slots, site)
                    }
                    _ => children,
                };
                Node::Field {
                    slot,
                    directives,
                    children,
                }
            }
            Node::Fragment {
                type_condition,
                directives,
                children,
            } => {
                let inner = type_condition.unwrap_or(parent_type);
                let children = grow_nodes(children, inner, slots, site);
                Node::Fragment {
                    type_condition,
                    directives,
                    children,
                }
            }
        };
        out.push(node);
        // A union scope admits only fragments, so the added leaf goes inside one.
        if here && !is_union(parent_type) {
            out.push(Node::Field {
                slot: 0,
                directives: DIRECTIVE_NONE,
                children: Vec::new(),
            });
        }
    }
    out
}

/// Removes one selection.
fn drop_nodes(
    nodes: Vec<Node>,
    parent_type: &'static str,
    slots: &[Slot],
    site: &mut Site,
) -> Vec<Node> {
    let mut out: Vec<Node> = Vec::new();
    for node in nodes {
        let here = if site.fired {
            false
        } else if site.remaining == 0 {
            site.fired = true;
            true
        } else {
            site.remaining -= 1;
            false
        };
        if here {
            continue;
        }
        let node = match node {
            Node::Field {
                slot,
                directives,
                children,
            } => {
                let children = match field_output_type(parent_type, slots[slot].field) {
                    // A composite field must keep a selection set, so its last child is not
                    // dropped; the property only needs *some* obligations removed.
                    Some(child_type) if children.len() > 1 => {
                        drop_nodes(children, child_type, slots, site)
                    }
                    _ => children,
                };
                Node::Field {
                    slot,
                    directives,
                    children,
                }
            }
            Node::Fragment {
                type_condition,
                directives,
                children,
            } => {
                let inner = type_condition.unwrap_or(parent_type);
                let children = if children.len() > 1 {
                    drop_nodes(children, inner, slots, site)
                } else {
                    children
                };
                Node::Fragment {
                    type_condition,
                    directives,
                    children,
                }
            }
        };
        out.push(node);
    }
    out
}

/// A base case and a weakened one: the left operation has gained selections and the right has
/// lost them. If `includes` held for the base it must hold for the weakened pair — the left
/// produces at least as much and the right demands at most as much.
pub struct WeakenedCase {
    pub base: Case,
    pub weakened: Case,
}

pub fn weakened_case(bytes: &[u8], seed: u64, steps: usize) -> WeakenedCase {
    let mut cursor = Cursor { bytes, position: 0 };
    let mut left = decode_operation(&mut cursor);
    let mut right = decode_operation(&mut cursor);
    left.declarations = Declarations::uniform();
    right.declarations = Declarations::uniform();
    let base = Case {
        left: render_operation(&left),
        right: render_operation(&right),
    };

    let mut state = seed | 1;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };

    for _ in 0..steps {
        let target = (next() % 6) as usize;
        if next() % 2 == 0 {
            let mut site = Site {
                remaining: target,
                fired: false,
            };
            left.selections = grow_nodes(left.selections, "Animal", &left.slots, &mut site);
        } else {
            let mut site = Site {
                remaining: target,
                fired: false,
            };
            // Never empty the root: an operation needs at least one selection.
            if right.selections.len() > 1 || right.selections.first().is_some_and(|node| {
                matches!(node, Node::Field { children, .. } | Node::Fragment { children, .. } if children.len() > 1)
            }) {
                right.selections = drop_nodes(right.selections, "Animal", &right.slots, &mut site);
            }
        }
    }

    WeakenedCase {
        base,
        weakened: Case {
            left: render_operation(&left),
            right: render_operation(&right),
        },
    }
}

/// Three operations decoded from one byte string, for the transitivity property.
pub fn triple(bytes: &[u8]) -> (String, String, String) {
    let mut cursor = Cursor { bytes, position: 0 };
    let mut operations = (0..3)
        .map(|_| {
            let mut operation = decode_operation(&mut cursor);
            operation.declarations = Declarations::uniform();
            render_operation(&operation)
        })
        .collect::<Vec<_>>();
    let third = operations.pop().unwrap();
    let second = operations.pop().unwrap();
    let first = operations.pop().unwrap();
    (first, second, third)
}
