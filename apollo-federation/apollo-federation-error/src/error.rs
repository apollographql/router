use crate::ast::AstNode;
use indexmap::IndexMap;
use lazy_static::lazy_static;
use std::fmt::{Display, Formatter, Write};
use strum::IntoEnumIterator;

// What we really needed here was the string representations in enum form, this isn't meant to
// replace AST components.
#[derive(Clone, Debug, strum_macros::Display, strum_macros::IntoStaticStr)]
pub enum SchemaRootKindEnum {
    #[strum(to_string = "query")]
    Query,
    #[strum(to_string = "mutation")]
    Mutation,
    #[strum(to_string = "subscription")]
    Subscription,
}

impl From<SchemaRootKindEnum> for String {
    fn from(value: SchemaRootKindEnum) -> Self {
        value.to_string()
    }
}

// TODO: graphql-js writes information about the location of the node, if one exists. Ideally, we
// should do something similar with AstNode, but will punt to later.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct SingleFederationError {
    pub code: String,
    pub message: String,
    pub nodes: Vec<AstNode>,
}

#[derive(Debug, thiserror::Error)]
pub struct MultipleFederationErrors {
    pub errors: Vec<SingleFederationError>,
}

impl Display for MultipleFederationErrors {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "The following errors occurred:")?;
        for error in &self.errors {
            write!(f, "\n\n  - ")?;
            for c in error.to_string().chars() {
                if c == '\n' {
                    write!(f, "\n    ")?;
                } else {
                    f.write_char(c)?;
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub struct AggregateFederationError {
    pub code: String,
    pub message: String,
    pub causes: Vec<SingleFederationError>,
}

impl Display for AggregateFederationError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}\ncaused by:", self.code, self.message)?;
        for error in &self.causes {
            write!(f, "\n\n  - ")?;
            for c in error.to_string().chars() {
                if c == '\n' {
                    write!(f, "\n    ")?;
                } else {
                    f.write_char(c)?;
                }
            }
        }
        Ok(())
    }
}

// PORT_NOTE: Often times, JS functions would either throw/return a GraphQLError, return a vector
// of GraphQLErrors, or take a vector of GraphQLErrors and group them together under an
// AggregateGraphQLError which itself would have a specific error message and code, and throw that.
// We represent all these cases with an enum, and delegate to the members.
#[derive(Debug, thiserror::Error)]
pub enum FederationError {
    #[error(transparent)]
    SingleFederationError(#[from] SingleFederationError),
    #[error(transparent)]
    MultipleFederationErrors(#[from] MultipleFederationErrors),
    #[error(transparent)]
    AggregateFederationError(#[from] AggregateFederationError),
}

/*
 * We didn't track errors addition precisely pre-2.0 and tracking it now has an
 * unclear ROI, so we just mark all the error code that predates 2.0 as 0.x.
 */
const FED1_CODE: &str = "0.x";

#[derive(Debug, Clone)]
pub struct ErrorCodeMetadata {
    pub added_in: &'static str,
    pub replaces: &'static [&'static str],
}

#[derive(Debug)]
pub struct ErrorCodeDefinition {
    code: String,
    // PORT_NOTE: Known as "description" in the JS code. The name was changed to distinguish it from
    // Error.description().
    doc_description: String,
    metadata: ErrorCodeMetadata,
}

impl ErrorCodeDefinition {
    fn new(code: String, doc_description: String, metadata: Option<ErrorCodeMetadata>) -> Self {
        Self {
            code,
            doc_description,
            metadata: metadata.unwrap_or_else(|| DEFAULT_METADATA.clone()),
        }
    }

    pub fn err(&self, message: String, nodes: Option<Vec<AstNode>>) -> SingleFederationError {
        let nodes = nodes.unwrap_or_default();
        SingleFederationError {
            code: self.code.clone(),
            message,
            nodes,
        }
    }

    pub fn code(&self) -> &str {
        &self.code
    }

    pub fn doc_description(&self) -> &str {
        &self.doc_description
    }

    pub fn metadata(&self) -> &ErrorCodeMetadata {
        &self.metadata
    }
}

/*
 * Most codes currently originate from the initial fed 2 release so we use this for convenience.
 * This can be changed later, inline versions everywhere, if that becomes irrelevant.
 */
static DEFAULT_METADATA: ErrorCodeMetadata = ErrorCodeMetadata {
    added_in: "2.0.0",
    replaces: &[],
};

pub struct ErrorCodeCategory<TElement: Clone + Into<String>> {
    // Fn(element: TElement) -> String
    extract_code: Box<dyn 'static + Send + Sync + Fn(TElement) -> String>,
    // Fn(element: TElement) -> String
    make_doc_description: Box<dyn 'static + Send + Sync + Fn(TElement) -> String>,
    metadata: ErrorCodeMetadata,
}

impl<TElement: Clone + Into<String>> ErrorCodeCategory<TElement> {
    fn new(
        extract_code: Box<dyn 'static + Send + Sync + Fn(TElement) -> String>,
        make_doc_description: Box<dyn 'static + Send + Sync + Fn(TElement) -> String>,
        metadata: Option<ErrorCodeMetadata>,
    ) -> Self {
        Self {
            extract_code,
            make_doc_description,
            metadata: metadata.unwrap_or_else(|| DEFAULT_METADATA.clone()),
        }
    }

    // PORT_NOTE: The Typescript type in the JS code only has get(), but I also added createCode()
    // here since it's used in the return type of makeErrorCodeCategory().
    fn create_code(&self, element: TElement) -> ErrorCodeDefinition {
        ErrorCodeDefinition::new(
            (self.extract_code)(element.clone()),
            (self.make_doc_description)(element),
            Some(self.metadata.clone()),
        )
    }

    pub fn get(&self, element: TElement) -> &ErrorCodeDefinition {
        let code = (self.extract_code)(element.clone());
        let def = CODE_DEF_BY_CODE.get(&code);
        def.unwrap_or_else(|| panic!("Unexpected element: {}", element.into()))
    }
}

impl ErrorCodeCategory<String> {
    fn new_federation_directive(
        code_suffix: String,
        make_doc_description: Box<dyn 'static + Send + Sync + Fn(String) -> String>,
        metadata: Option<ErrorCodeMetadata>,
    ) -> Self {
        Self::new(
            Box::new(move |element: String| format!("{}_{}", element.to_uppercase(), code_suffix)),
            make_doc_description,
            metadata,
        )
    }
}

lazy_static! {
    static ref INVALID_GRAPHQL: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "INVALID_GRAPHQL".to_owned(),
        "A schema is invalid GraphQL: it violates one of the rule of the specification.".to_owned(),
        None,
    );

    static ref DIRECTIVE_DEFINITION_INVALID: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "DIRECTIVE_DEFINITION_INVALID".to_owned(),
        "A built-in or federation directive has an invalid definition in the schema.".to_owned(),
        Some(ErrorCodeMetadata {
            replaces: &["TAG_DEFINITION_INVALID"],
            ..DEFAULT_METADATA.clone()
        }),
    );

    static ref TYPE_DEFINITION_INVALID: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "TYPE_DEFINITION_INVALID".to_owned(),
        "A built-in or federation type has an invalid definition in the schema.".to_owned(),
        None,
    );

    static ref UNSUPPORTED_LINKED_FEATURE: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "UNSUPPORTED_LINKED_FEATURE".to_owned(),
        "Indicates that a feature used in a @link is either unsupported or is used with unsupported options.".to_owned(),
        None,
    );

    static ref UNKNOWN_FEDERATION_LINK_VERSION: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "UNKNOWN_FEDERATION_LINK_VERSION".to_owned(),
        "The version of federation in a @link directive on the schema is unknown.".to_owned(),
        None,
    );

    static ref UNKNOWN_LINK_VERSION: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "UNKNOWN_LINK_VERSION".to_owned(),
        "The version of @link set on the schema is unknown.".to_owned(),
        Some(ErrorCodeMetadata {
            added_in: "2.1.0",
            replaces: &[],
        }),
    );

    static ref FIELDS_HAS_ARGS: ErrorCodeCategory<String> = ErrorCodeCategory::new_federation_directive(
        "FIELDS_HAS_ARGS".to_owned(),
        Box::new(|directive| format!("The `fields` argument of a `@{}` directive includes a field defined with arguments (which is not currently supported).", directive)),
        None,
    );

    static ref KEY_FIELDS_HAS_ARGS: ErrorCodeDefinition = FIELDS_HAS_ARGS.create_code("key".to_owned());
    static ref PROVIDES_FIELDS_HAS_ARGS: ErrorCodeDefinition = FIELDS_HAS_ARGS.create_code("provides".to_owned());

    static ref DIRECTIVE_FIELDS_MISSING_EXTERNAL: ErrorCodeCategory<String> = ErrorCodeCategory::new_federation_directive(
        "FIELDS_MISSING_EXTERNAL".to_owned(),
        Box::new(|directive| format!("The `fields` argument of a `@{}` directive includes a field that is not marked as `@external`.", directive)),
        Some(ErrorCodeMetadata {
            added_in: FED1_CODE,
            replaces: &[],
        }),
    );

    static ref PROVIDES_MISSING_EXTERNAL: ErrorCodeDefinition =
        DIRECTIVE_FIELDS_MISSING_EXTERNAL.create_code("provides".to_owned());
    static ref REQUIRES_MISSING_EXTERNAL: ErrorCodeDefinition =
        DIRECTIVE_FIELDS_MISSING_EXTERNAL.create_code("requires".to_owned());

    static ref DIRECTIVE_UNSUPPORTED_ON_INTERFACE: ErrorCodeCategory<String> = ErrorCodeCategory::new_federation_directive(
        "UNSUPPORTED_ON_INTERFACE".to_owned(),
        Box::new(|directive| {
            let suffix = if directive == "key" {
                "only supported when @linking to federation 2.3+"
            } else {
                "not (yet) supported"
            };
            format!(
                "A `@{}` directive is used on an interface, which is {}.",
                directive, suffix
            )
        }),
        None,
    );

    static ref KEY_UNSUPPORTED_ON_INTERFACE: ErrorCodeDefinition =
        DIRECTIVE_UNSUPPORTED_ON_INTERFACE.create_code("key".to_owned());
    static ref PROVIDES_UNSUPPORTED_ON_INTERFACE: ErrorCodeDefinition =
        DIRECTIVE_UNSUPPORTED_ON_INTERFACE.create_code("provides".to_owned());
    static ref REQUIRES_UNSUPPORTED_ON_INTERFACE: ErrorCodeDefinition =
        DIRECTIVE_UNSUPPORTED_ON_INTERFACE.create_code("requires".to_owned());

    static ref DIRECTIVE_IN_FIELDS_ARG: ErrorCodeCategory<String> = ErrorCodeCategory::new_federation_directive(
        "DIRECTIVE_IN_FIELDS_ARG".to_owned(),
        Box::new(|directive| format!("The `fields` argument of a `@{}` directive includes some directive applications. This is not supported", directive)),
        Some(ErrorCodeMetadata {
            added_in: "2.1.0",
            replaces: &[],
        }),
    );

    static ref KEY_HAS_DIRECTIVE_IN_FIELDS_ARGS: ErrorCodeDefinition = DIRECTIVE_IN_FIELDS_ARG.create_code("key".to_owned());
    static ref PROVIDES_HAS_DIRECTIVE_IN_FIELDS_ARGS: ErrorCodeDefinition = DIRECTIVE_IN_FIELDS_ARG.create_code("provides".to_owned());
    static ref REQUIRES_HAS_DIRECTIVE_IN_FIELDS_ARGS: ErrorCodeDefinition = DIRECTIVE_IN_FIELDS_ARG.create_code("requires".to_owned());

    static ref EXTERNAL_UNUSED: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "EXTERNAL_UNUSED".to_owned(),
        "An `@external` field is not being used by any instance of `@key`, `@requires`, `@provides` or to satisfy an interface implementation.".to_owned(),
        Some(ErrorCodeMetadata {
            added_in: FED1_CODE,
            replaces: &[],
        }),
    );

    static ref TYPE_WITH_ONLY_UNUSED_EXTERNAL: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "TYPE_WITH_ONLY_UNUSED_EXTERNAL".to_owned(),
        [
            "A federation 1 schema has a composite type comprised only of unused external fields.".to_owned(),
            format!("Note that this error can _only_ be raised for federation 1 schema as federation 2 schema do not allow unused external fields (and errors with code {} will be raised in that case).", EXTERNAL_UNUSED.code),
            "But when federation 1 schema are automatically migrated to federation 2 ones, unused external fields are automatically removed, and in rare case this can leave a type empty. If that happens, an error with this code will be raised".to_owned()
        ].join(" "),
        None,
    );

    static ref PROVIDES_ON_NON_OBJECT_FIELD: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "PROVIDES_ON_NON_OBJECT_FIELD".to_owned(),
        "A `@provides` directive is used to mark a field whose base type is not an object type.".to_owned(),
        None,
    );

    static ref DIRECTIVE_INVALID_FIELDS_TYPE: ErrorCodeCategory<String> = ErrorCodeCategory::new_federation_directive(
        "INVALID_FIELDS_TYPE".to_owned(),
        Box::new(|directive| format!("The value passed to the `fields` argument of a `@{}` directive is not a string.", directive)),
        None,
    );

    static ref KEY_INVALID_FIELDS_TYPE: ErrorCodeDefinition =
        DIRECTIVE_INVALID_FIELDS_TYPE.create_code("key".to_owned());
    static ref PROVIDES_INVALID_FIELDS_TYPE: ErrorCodeDefinition =
        DIRECTIVE_INVALID_FIELDS_TYPE.create_code("provides".to_owned());
    static ref REQUIRES_INVALID_FIELDS_TYPE: ErrorCodeDefinition =
        DIRECTIVE_INVALID_FIELDS_TYPE.create_code("requires".to_owned());

    static ref DIRECTIVE_INVALID_FIELDS: ErrorCodeCategory<String> = ErrorCodeCategory::new_federation_directive(
        "INVALID_FIELDS".to_owned(),
        Box::new(|directive| format!("The `fields` argument of a `@{}` directive is invalid (it has invalid syntax, includes unknown fields, ...).", directive)),
        None,
    );

    static ref KEY_INVALID_FIELDS: ErrorCodeDefinition =
        DIRECTIVE_INVALID_FIELDS.create_code("key".to_owned());
    static ref PROVIDES_INVALID_FIELDS: ErrorCodeDefinition =
        DIRECTIVE_INVALID_FIELDS.create_code("provides".to_owned());
    static ref REQUIRES_INVALID_FIELDS: ErrorCodeDefinition =
        DIRECTIVE_INVALID_FIELDS.create_code("requires".to_owned());

    static ref KEY_FIELDS_SELECT_INVALID_TYPE: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "KEY_FIELDS_SELECT_INVALID_TYPE".to_owned(),
        "The `fields` argument of `@key` directive includes a field whose type is a list, interface, or union type. Fields of these types cannot be part of a `@key`".to_owned(),
        Some(ErrorCodeMetadata {
            added_in: FED1_CODE,
            replaces: &[],
        }),
    );

    static ref ROOT_TYPE_USED: ErrorCodeCategory<SchemaRootKindEnum> = ErrorCodeCategory::new(
        Box::new(|element| {
            let kind: String = element.into();
            format!("ROOT_{}_USED", kind.to_uppercase())
        }),
        Box::new(|element| {
            let kind: String = element.into();
            format!("A subgraph's schema defines a type with the name `{}`, while also specifying a _different_ type name as the root query object. This is not allowed.", kind)
        }),
        Some(ErrorCodeMetadata {
            added_in: FED1_CODE,
            replaces: &[],
        })

    );

    static ref ROOT_QUERY_USED: ErrorCodeDefinition = ROOT_TYPE_USED.create_code(SchemaRootKindEnum::Query);
    static ref ROOT_MUTATION_USED: ErrorCodeDefinition = ROOT_TYPE_USED.create_code(SchemaRootKindEnum::Mutation);
    static ref ROOT_SUBSCRIPTION_USED: ErrorCodeDefinition = ROOT_TYPE_USED.create_code(SchemaRootKindEnum::Subscription);

    static ref INVALID_SUBGRAPH_NAME: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "INVALID_SUBGRAPH_NAME".to_owned(),
        "A subgraph name is invalid (subgraph names cannot be a single underscore (\"_\")).".to_owned(),
        None,
    );

    static ref NO_QUERIES: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "NO_QUERIES".to_owned(),
        "None of the composed subgraphs expose any query.".to_owned(),
        None,
    );

    static ref INTERFACE_FIELD_NO_IMPLEM: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "INTERFACE_FIELD_NO_IMPLEM".to_owned(),
        "After subgraph merging, an implementation is missing a field of one of the interface it implements (which can happen for valid subgraphs).".to_owned(),
        None,
    );

    static ref TYPE_KIND_MISMATCH: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "TYPE_KIND_MISMATCH".to_owned(),
        "A type has the same name in different subgraphs, but a different kind. For instance, one definition is an object type but another is an interface.".to_owned(),
        Some(ErrorCodeMetadata {
            replaces: &["VALUE_TYPE_KIND_MISMATCH", "EXTENSION_OF_WRONG_KIND", "ENUM_MISMATCH_TYPE"],
            ..DEFAULT_METADATA.clone()
        }),
    );

    static ref EXTERNAL_TYPE_MISMATCH: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "EXTERNAL_TYPE_MISMATCH".to_owned(),
        "An `@external` field has a type that is incompatible with the declaration(s) of that field in other subgraphs.".to_owned(),
        Some(ErrorCodeMetadata {
            added_in: FED1_CODE,
            replaces: &[],
        }),
    );

    static ref EXTERNAL_COLLISION_WITH_ANOTHER_DIRECTIVE: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "EXTERNAL_COLLISION_WITH_ANOTHER_DIRECTIVE".to_owned(),
        "The @external directive collides with other directives in some situations.".to_owned(),
        Some(ErrorCodeMetadata {
            added_in: "2.1.0",
            replaces: &[],
        }),
    );

    static ref EXTERNAL_ARGUMENT_MISSING: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "EXTERNAL_ARGUMENT_MISSING".to_owned(),
        "An `@external` field is missing some arguments present in the declaration(s) of that field in other subgraphs.".to_owned(),
        None,
    );

    static ref EXTERNAL_ARGUMENT_TYPE_MISMATCH: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "EXTERNAL_ARGUMENT_TYPE_MISMATCH".to_owned(),
        "An `@external` field declares an argument with a type that is incompatible with the corresponding argument in the declaration(s) of that field in other subgraphs.".to_owned(),
        None,
    );


    static ref EXTERNAL_ARGUMENT_DEFAULT_MISMATCH: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "EXTERNAL_ARGUMENT_DEFAULT_MISMATCH".to_owned(),
        "An `@external` field declares an argument with a default that is incompatible with the corresponding argument in the declaration(s) of that field in other subgraphs.".to_owned(),
        None,
    );

    static ref EXTERNAL_ON_INTERFACE: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "EXTERNAL_ON_INTERFACE".to_owned(),
        "The field of an interface type is marked with `@external`: as external is about marking field not resolved by the subgraph and as interface field are not resolved (only implementations of those fields are), an \"external\" interface field is nonsensical".to_owned(),
        None,
    );

    static ref MERGED_DIRECTIVE_APPLICATION_ON_EXTERNAL: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "MERGED_DIRECTIVE_APPLICATION_ON_EXTERNAL".to_owned(),
        "In a subgraph, a field is both marked @external and has a merged directive applied to it".to_owned(),
        None,
    );

    static ref FIELD_TYPE_MISMATCH: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "FIELD_TYPE_MISMATCH".to_owned(),
        "A field has a type that is incompatible with other declarations of that field in other subgraphs.".to_owned(),
        Some(ErrorCodeMetadata {
            replaces: &["VALUE_TYPE_FIELD_TYPE_MISMATCH"],
            ..DEFAULT_METADATA.clone()
        }),
    );

    static ref ARGUMENT_TYPE_MISMATCH: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "FIELD_ARGUMENT_TYPE_MISMATCH".to_owned(),
        "An argument (of a field/directive) has a type that is incompatible with that of other declarations of that same argument in other subgraphs.".to_owned(),
        Some(ErrorCodeMetadata {
            replaces: &["VALUE_TYPE_INPUT_VALUE_MISMATCH"],
            ..DEFAULT_METADATA.clone()
        }),
    );

    static ref INPUT_FIELD_DEFAULT_MISMATCH: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "INPUT_FIELD_DEFAULT_MISMATCH".to_owned(),
        "An input field has a default value that is incompatible with other declarations of that field in other subgraphs.".to_owned(),
        None,
    );

    static ref ARGUMENT_DEFAULT_MISMATCH: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "FIELD_ARGUMENT_DEFAULT_MISMATCH".to_owned(),
        "An argument (of a field/directive) has a default value that is incompatible with that of other declarations of that same argument in other subgraphs.".to_owned(),
        None,
    );

    static ref EXTENSION_WITH_NO_BASE: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "EXTENSION_WITH_NO_BASE".to_owned(),
        "A subgraph is attempting to `extend` a type that is not originally defined in any known subgraph.".to_owned(),
        Some(ErrorCodeMetadata {
            added_in: FED1_CODE,
            replaces: &[],
        }),
    );

    static ref EXTERNAL_MISSING_ON_BASE: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "EXTERNAL_MISSING_ON_BASE".to_owned(),
        "A field is marked as `@external` in a subgraph but with no non-external declaration in any other subgraph.".to_owned(),
        Some(ErrorCodeMetadata {
            added_in: FED1_CODE,
            replaces: &[],
        }),
    );

    static ref INVALID_FIELD_SHARING: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "INVALID_FIELD_SHARING".to_owned(),
        "A field that is non-shareable in at least one subgraph is resolved by multiple subgraphs.".to_owned(),
        None,
    );

    static ref INVALID_SHAREABLE_USAGE: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "INVALID_SHAREABLE_USAGE".to_owned(),
        "The `@shareable` federation directive is used in an invalid way.".to_owned(),
        Some(ErrorCodeMetadata {
            added_in: "2.1.2",
            replaces: &[],
        }),
    );

    static ref INVALID_LINK_DIRECTIVE_USAGE: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "INVALID_LINK_DIRECTIVE_USAGE".to_owned(),
        "An application of the @link directive is invalid/does not respect the specification.".to_owned(),
        None,
    );

    static ref INVALID_LINK_IDENTIFIER: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "INVALID_LINK_IDENTIFIER".to_owned(),
        "A url/version for a @link feature is invalid/does not respect the specification.".to_owned(),
        Some(ErrorCodeMetadata {
            added_in: "2.1.0",
            replaces: &[],
        }),
    );

    static ref LINK_IMPORT_NAME_MISMATCH: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "LINK_IMPORT_NAME_MISMATCH".to_owned(),
        "The import name for a merged directive (as declared by the relevant `@link(import:)` argument) is inconsistent between subgraphs.".to_owned(),
        None,
    );

    static ref REFERENCED_INACCESSIBLE: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "REFERENCED_INACCESSIBLE".to_owned(),
        "An element is marked as @inaccessible but is referenced by an element visible in the API schema.".to_owned(),
        None,
    );

    static ref DEFAULT_VALUE_USES_INACCESSIBLE: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "DEFAULT_VALUE_USES_INACCESSIBLE".to_owned(),
        "An element is marked as @inaccessible but is used in the default value of an element visible in the API schema.".to_owned(),
        None,
    );

    static ref QUERY_ROOT_TYPE_INACCESSIBLE: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "QUERY_ROOT_TYPE_INACCESSIBLE".to_owned(),
        "An element is marked as @inaccessible but is the query root type, which must be visible in the API schema.".to_owned(),
        None,
    );

    static ref REQUIRED_INACCESSIBLE: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "REQUIRED_INACCESSIBLE".to_owned(),
        "An element is marked as @inaccessible but is required by an element visible in the API schema.".to_owned(),
        None,
    );

    static ref IMPLEMENTED_BY_INACCESSIBLE: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "IMPLEMENTED_BY_INACCESSIBLE".to_owned(),
        "An element is marked as @inaccessible but implements an element visible in the API schema.".to_owned(),
        None,
    );
}

// The above lazy_static! block hits recursion limit if we try to add more to it, so we start a
// new block here.
lazy_static! {
    static ref DISALLOWED_INACCESSIBLE: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "DISALLOWED_INACCESSIBLE".to_owned(),
        "An element is marked as @inaccessible that is not allowed to be @inaccessible.".to_owned(),
        None,
    );

    static ref ONLY_INACCESSIBLE_CHILDREN: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "ONLY_INACCESSIBLE_CHILDREN".to_owned(),
        "A type visible in the API schema has only @inaccessible children.".to_owned(),
        None,
    );

    static ref REQUIRED_INPUT_FIELD_MISSING_IN_SOME_SUBGRAPH: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "REQUIRED_INPUT_FIELD_MISSING_IN_SOME_SUBGRAPH".to_owned(),
        "A field of an input object type is mandatory in some subgraphs, but the field is not defined in all the subgraphs that define the input object type.".to_owned(),
        None,
    );

    static ref REQUIRED_ARGUMENT_MISSING_IN_SOME_SUBGRAPH: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "REQUIRED_ARGUMENT_MISSING_IN_SOME_SUBGRAPH".to_owned(),
        "An argument of a field or directive definition is mandatory in some subgraphs, but the argument is not defined in all the subgraphs that define the field or directive definition.".to_owned(),
        None,
    );

    static ref EMPTY_MERGED_INPUT_TYPE: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "EMPTY_MERGED_INPUT_TYPE".to_owned(),
        "An input object type has no field common to all the subgraphs that define the type. Merging that type would result in an invalid empty input object type.".to_owned(),
        None,
    );

    static ref ENUM_VALUE_MISMATCH: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "ENUM_VALUE_MISMATCH".to_owned(),
        "An enum type that is used as both an input and output type has a value that is not defined in all the subgraphs that define the enum type.".to_owned(),
        None,
    );

    static ref EMPTY_MERGED_ENUM_TYPE: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "EMPTY_MERGED_ENUM_TYPE".to_owned(),
        "An enum type has no value common to all the subgraphs that define the type. Merging that type would result in an invalid empty enum type.".to_owned(),
        None,
    );

    static ref SHAREABLE_HAS_MISMATCHED_RUNTIME_TYPES: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "SHAREABLE_HAS_MISMATCHED_RUNTIME_TYPES".to_owned(),
        "A shareable field return type has mismatched possible runtime types in the subgraphs in which the field is declared. As shared fields must resolve the same way in all subgraphs, this is almost surely a mistake.".to_owned(),
        None,
    );

    static ref SATISFIABILITY_ERROR: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "SATISFIABILITY_ERROR".to_owned(),
        "Subgraphs can be merged, but the resulting supergraph API would have queries that cannot be satisfied by those subgraphs.".to_owned(),
        None,
    );

    static ref OVERRIDE_FROM_SELF_ERROR: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "OVERRIDE_FROM_SELF_ERROR".to_owned(),
        "Field with `@override` directive has \"from\" location that references its own subgraph.".to_owned(),
        None,
    );

    static ref OVERRIDE_SOURCE_HAS_OVERRIDE: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "OVERRIDE_SOURCE_HAS_OVERRIDE".to_owned(),
        "Field which is overridden to another subgraph is also marked @override.".to_owned(),
        None,
    );

    static ref OVERRIDE_COLLISION_WITH_ANOTHER_DIRECTIVE: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "OVERRIDE_COLLISION_WITH_ANOTHER_DIRECTIVE".to_owned(),
        "The @override directive cannot be used on external fields, nor to override fields with either @external, @provides, or @requires.".to_owned(),
        None,
    );

    static ref OVERRIDE_ON_INTERFACE: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "OVERRIDE_ON_INTERFACE".to_owned(),
        "The @override directive cannot be used on the fields of an interface type.".to_owned(),
        Some(ErrorCodeMetadata {
            added_in: "2.3.0",
            replaces: &[],
        }),
    );

    static ref UNSUPPORTED_FEATURE: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "UNSUPPORTED_FEATURE".to_owned(),
        "Indicates an error due to feature currently unsupported by federation.".to_owned(),
        Some(ErrorCodeMetadata {
            added_in: "2.1.0",
            replaces: &[],
        }),
    );

    static ref INVALID_FEDERATION_SUPERGRAPH: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "INVALID_FEDERATION_SUPERGRAPH".to_owned(),
        "Indicates that a schema provided for an Apollo Federation supergraph is not a valid supergraph schema.".to_owned(),
        Some(ErrorCodeMetadata {
            added_in: "2.1.0",
            replaces: &[],
        }),
    );

    static ref DOWNSTREAM_SERVICE_ERROR: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "DOWNSTREAM_SERVICE_ERROR".to_owned(),
        "Indicates an error in a subgraph service query during query execution in a federated service.".to_owned(),
        Some(ErrorCodeMetadata {
            added_in: FED1_CODE,
            replaces: &[],
        }),
    );

    static ref DIRECTIVE_COMPOSITION_ERROR: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "DIRECTIVE_COMPOSITION_ERROR".to_owned(),
        "Error when composing custom directives.".to_owned(),
        Some(ErrorCodeMetadata {
            added_in: "2.1.0",
            replaces: &[],
        }),
    );

    static ref INTERFACE_OBJECT_USAGE_ERROR: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "INTERFACE_OBJECT_USAGE_ERROR".to_owned(),
        "Error in the usage of the @interfaceObject directive.".to_owned(),
        Some(ErrorCodeMetadata {
            added_in: "2.3.0",
            replaces: &[],
        }),
    );

    static ref INTERFACE_KEY_NOT_ON_IMPLEMENTATION: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "INTERFACE_KEY_NOT_ON_IMPLEMENTATION".to_owned(),
        "A `@key` is defined on an interface type, but is not defined (or is not resolvable) on at least one of the interface implementations".to_owned(),
        Some(ErrorCodeMetadata {
            added_in: "2.3.0",
            replaces: &[],
        }),
    );

    static ref INTERFACE_KEY_MISSING_IMPLEMENTATION_TYPE: ErrorCodeDefinition = ErrorCodeDefinition::new(
        "INTERFACE_KEY_MISSING_IMPLEMENTATION_TYPE".to_owned(),
        "A subgraph has a `@key` on an interface type, but that subgraph does not define an implementation (in the supergraph) of that interface".to_owned(),
        Some(ErrorCodeMetadata {
            added_in: "2.3.0",
            replaces: &[],
        }),
    );
}

// PORT_NOTE: In the JS code, there's just one object named ERROR_CATEGORIES, but it has
// heterogeneous type (some of the keys have type ErrorCodeDefinition<String>, others have type
// ErrorCodeDefinition<SchemaRootKind>). We can't really represent that in Rust cleanly, so we just
// separate them into different enums.
#[derive(Debug)]
pub enum FederationDirectiveErrorCategory {
    DirectiveFieldsMissingExternal,
    DirectiveUnsupportedOnInterface,
    DirectiveInvalidFieldsType,
    DirectiveInvalidFields,
    FieldsHasArgs,
    DirectiveInFieldsArg,
}

impl FederationDirectiveErrorCategory {
    pub fn definition(&self) -> &'static ErrorCodeCategory<String> {
        match self {
            FederationDirectiveErrorCategory::DirectiveFieldsMissingExternal => {
                &DIRECTIVE_FIELDS_MISSING_EXTERNAL
            }
            FederationDirectiveErrorCategory::DirectiveUnsupportedOnInterface => {
                &DIRECTIVE_UNSUPPORTED_ON_INTERFACE
            }
            FederationDirectiveErrorCategory::DirectiveInvalidFieldsType => {
                &DIRECTIVE_INVALID_FIELDS_TYPE
            }
            FederationDirectiveErrorCategory::DirectiveInvalidFields => &DIRECTIVE_INVALID_FIELDS,
            FederationDirectiveErrorCategory::FieldsHasArgs => &FIELDS_HAS_ARGS,
            FederationDirectiveErrorCategory::DirectiveInFieldsArg => &DIRECTIVE_IN_FIELDS_ARG,
        }
    }
}

#[derive(Debug)]
pub enum SchemaRootKindErrorCategory {
    RootTypeUsed,
}

impl SchemaRootKindErrorCategory {
    pub fn definition(&self) -> &'static ErrorCodeCategory<SchemaRootKindEnum> {
        match self {
            SchemaRootKindErrorCategory::RootTypeUsed => &ROOT_TYPE_USED,
        }
    }
}

#[derive(Debug, strum_macros::EnumIter)]
pub enum ErrorCode {
    InvalidGraphQL,
    DirectiveDefinitionInvalid,
    TypeDefinitionInvalid,
    UnsupportedLinkedFeature,
    UnknownFederationLinkVersion,
    UnknownLinkVersion,
    KeyFieldsHasArgs,
    ProvidesFieldsHasArgs,
    ProvidesMissingExternal,
    RequiresMissingExternal,
    KeyUnsupportedOnInterface,
    ProvidesUnsupportedOnInterface,
    RequiresUnsupportedOnInterface,
    ExternalUnused,
    ExternalCollisionWithAnotherDirective,
    TypeWithOnlyUnusedExternal,
    ProvidesOnNonObjectField,
    KeyInvalidFieldsType,
    ProvidesInvalidFieldsType,
    RequiresInvalidFieldsType,
    KeyInvalidFields,
    ProvidesInvalidFields,
    RequiresInvalidFields,
    KeyFieldsSelectInvalidType,
    RootQueryUsed,
    RootMutationUsed,
    RootSubscriptionUsed,
    InvalidSubgraphName,
    NoQueries,
    InterfaceFieldNoImplem,
    TypeKindMismatch,
    ExternalTypeMismatch,
    ExternalArgumentMissing,
    ExternalArgumentTypeMismatch,
    ExternalArgumentDefaultMismatch,
    ExternalOnInterface,
    MergedDirectiveApplicationOnExternal,
    FieldTypeMismatch,
    ArgumentTypeMismatch,
    InputFieldDefaultMismatch,
    ArgumentDefaultMismatch,
    ExtensionWithNoBase,
    ExternalMissingOnBase,
    InvalidFieldSharing,
    InvalidShareableUsage,
    InvalidLinkDirectiveUsage,
    InvalidLinkIdentifier,
    LinkImportNameMismatch,
    ReferencedInaccessible,
    DefaultValueUsesInaccessible,
    QueryRootTypeInaccessible,
    RequiredInaccessible,
    DisallowedInaccessible,
    ImplementedByInaccessible,
    OnlyInaccessibleChildren,
    RequiredArgumentMissingInSomeSubgraph,
    RequiredInputFieldMissingInSomeSubgraph,
    EmptyMergedInputType,
    EnumValueMismatch,
    EmptyMergedEnumType,
    ShareableHasMismatchedRuntimeTypes,
    SatisfiabilityError,
    OverrideCollisionWithAnotherDirective,
    OverrideFromSelfError,
    OverrideSourceHasOverride,
    OverrideOnInterface,
    UnsupportedFeature,
    InvalidFederationSupergraph,
    DownstreamServiceError,
    KeyHasDirectiveInFieldsArgs,
    ProvidesHasDirectiveInFieldsArgs,
    RequiresHasDirectiveInFieldsArgs,
    DirectiveCompositionError,
    InterfaceObjectUsageError,
    InterfaceKeyNotOnImplementation,
    InterfaceKeyMissingImplementationType,
}

impl ErrorCode {
    pub fn definition(&self) -> &'static ErrorCodeDefinition {
        match self {
            ErrorCode::InvalidGraphQL => &INVALID_GRAPHQL,
            ErrorCode::DirectiveDefinitionInvalid => &DIRECTIVE_DEFINITION_INVALID,
            ErrorCode::TypeDefinitionInvalid => &TYPE_DEFINITION_INVALID,
            ErrorCode::UnsupportedLinkedFeature => &UNSUPPORTED_LINKED_FEATURE,
            ErrorCode::UnknownFederationLinkVersion => &UNKNOWN_FEDERATION_LINK_VERSION,
            ErrorCode::UnknownLinkVersion => &UNKNOWN_LINK_VERSION,
            ErrorCode::KeyFieldsHasArgs => &KEY_FIELDS_HAS_ARGS,
            ErrorCode::ProvidesFieldsHasArgs => &PROVIDES_FIELDS_HAS_ARGS,
            ErrorCode::ProvidesMissingExternal => &PROVIDES_MISSING_EXTERNAL,
            ErrorCode::RequiresMissingExternal => &REQUIRES_MISSING_EXTERNAL,
            ErrorCode::KeyUnsupportedOnInterface => &KEY_UNSUPPORTED_ON_INTERFACE,
            ErrorCode::ProvidesUnsupportedOnInterface => &PROVIDES_UNSUPPORTED_ON_INTERFACE,
            ErrorCode::RequiresUnsupportedOnInterface => &REQUIRES_UNSUPPORTED_ON_INTERFACE,
            ErrorCode::ExternalUnused => &EXTERNAL_UNUSED,
            ErrorCode::ExternalCollisionWithAnotherDirective => {
                &EXTERNAL_COLLISION_WITH_ANOTHER_DIRECTIVE
            }
            ErrorCode::TypeWithOnlyUnusedExternal => &TYPE_WITH_ONLY_UNUSED_EXTERNAL,
            ErrorCode::ProvidesOnNonObjectField => &PROVIDES_ON_NON_OBJECT_FIELD,
            ErrorCode::KeyInvalidFieldsType => &KEY_INVALID_FIELDS_TYPE,
            ErrorCode::ProvidesInvalidFieldsType => &PROVIDES_INVALID_FIELDS_TYPE,
            ErrorCode::RequiresInvalidFieldsType => &REQUIRES_INVALID_FIELDS_TYPE,
            ErrorCode::KeyInvalidFields => &KEY_INVALID_FIELDS,
            ErrorCode::ProvidesInvalidFields => &PROVIDES_INVALID_FIELDS,
            ErrorCode::RequiresInvalidFields => &REQUIRES_INVALID_FIELDS,
            ErrorCode::KeyFieldsSelectInvalidType => &KEY_FIELDS_SELECT_INVALID_TYPE,
            ErrorCode::RootQueryUsed => &ROOT_QUERY_USED,
            ErrorCode::RootMutationUsed => &ROOT_MUTATION_USED,
            ErrorCode::RootSubscriptionUsed => &ROOT_SUBSCRIPTION_USED,
            ErrorCode::InvalidSubgraphName => &INVALID_SUBGRAPH_NAME,
            ErrorCode::NoQueries => &NO_QUERIES,
            ErrorCode::InterfaceFieldNoImplem => &INTERFACE_FIELD_NO_IMPLEM,
            ErrorCode::TypeKindMismatch => &TYPE_KIND_MISMATCH,
            ErrorCode::ExternalTypeMismatch => &EXTERNAL_TYPE_MISMATCH,
            ErrorCode::ExternalArgumentMissing => &EXTERNAL_ARGUMENT_MISSING,
            ErrorCode::ExternalArgumentTypeMismatch => &EXTERNAL_ARGUMENT_TYPE_MISMATCH,
            ErrorCode::ExternalArgumentDefaultMismatch => &EXTERNAL_ARGUMENT_DEFAULT_MISMATCH,
            ErrorCode::ExternalOnInterface => &EXTERNAL_ON_INTERFACE,
            ErrorCode::MergedDirectiveApplicationOnExternal => {
                &MERGED_DIRECTIVE_APPLICATION_ON_EXTERNAL
            }
            ErrorCode::FieldTypeMismatch => &FIELD_TYPE_MISMATCH,
            ErrorCode::ArgumentTypeMismatch => &ARGUMENT_TYPE_MISMATCH,
            ErrorCode::InputFieldDefaultMismatch => &INPUT_FIELD_DEFAULT_MISMATCH,
            ErrorCode::ArgumentDefaultMismatch => &ARGUMENT_DEFAULT_MISMATCH,
            ErrorCode::ExtensionWithNoBase => &EXTENSION_WITH_NO_BASE,
            ErrorCode::ExternalMissingOnBase => &EXTERNAL_MISSING_ON_BASE,
            ErrorCode::InvalidFieldSharing => &INVALID_FIELD_SHARING,
            ErrorCode::InvalidShareableUsage => &INVALID_SHAREABLE_USAGE,
            ErrorCode::InvalidLinkDirectiveUsage => &INVALID_LINK_DIRECTIVE_USAGE,
            ErrorCode::InvalidLinkIdentifier => &INVALID_LINK_IDENTIFIER,
            ErrorCode::LinkImportNameMismatch => &LINK_IMPORT_NAME_MISMATCH,
            ErrorCode::ReferencedInaccessible => &REFERENCED_INACCESSIBLE,
            ErrorCode::DefaultValueUsesInaccessible => &DEFAULT_VALUE_USES_INACCESSIBLE,
            ErrorCode::QueryRootTypeInaccessible => &QUERY_ROOT_TYPE_INACCESSIBLE,
            ErrorCode::RequiredInaccessible => &REQUIRED_INACCESSIBLE,
            ErrorCode::DisallowedInaccessible => &DISALLOWED_INACCESSIBLE,
            ErrorCode::ImplementedByInaccessible => &IMPLEMENTED_BY_INACCESSIBLE,
            ErrorCode::OnlyInaccessibleChildren => &ONLY_INACCESSIBLE_CHILDREN,
            ErrorCode::RequiredArgumentMissingInSomeSubgraph => {
                &REQUIRED_ARGUMENT_MISSING_IN_SOME_SUBGRAPH
            }
            ErrorCode::RequiredInputFieldMissingInSomeSubgraph => {
                &REQUIRED_INPUT_FIELD_MISSING_IN_SOME_SUBGRAPH
            }
            ErrorCode::EmptyMergedInputType => &EMPTY_MERGED_INPUT_TYPE,
            ErrorCode::EnumValueMismatch => &ENUM_VALUE_MISMATCH,
            ErrorCode::EmptyMergedEnumType => &EMPTY_MERGED_ENUM_TYPE,
            ErrorCode::ShareableHasMismatchedRuntimeTypes => {
                &SHAREABLE_HAS_MISMATCHED_RUNTIME_TYPES
            }
            ErrorCode::SatisfiabilityError => &SATISFIABILITY_ERROR,
            ErrorCode::OverrideCollisionWithAnotherDirective => {
                &OVERRIDE_COLLISION_WITH_ANOTHER_DIRECTIVE
            }
            ErrorCode::OverrideFromSelfError => &OVERRIDE_FROM_SELF_ERROR,
            ErrorCode::OverrideSourceHasOverride => &OVERRIDE_SOURCE_HAS_OVERRIDE,
            ErrorCode::OverrideOnInterface => &OVERRIDE_ON_INTERFACE,
            ErrorCode::UnsupportedFeature => &UNSUPPORTED_FEATURE,
            ErrorCode::InvalidFederationSupergraph => &INVALID_FEDERATION_SUPERGRAPH,
            ErrorCode::DownstreamServiceError => &DOWNSTREAM_SERVICE_ERROR,
            ErrorCode::KeyHasDirectiveInFieldsArgs => &KEY_HAS_DIRECTIVE_IN_FIELDS_ARGS,
            ErrorCode::ProvidesHasDirectiveInFieldsArgs => &PROVIDES_HAS_DIRECTIVE_IN_FIELDS_ARGS,
            ErrorCode::RequiresHasDirectiveInFieldsArgs => &REQUIRES_HAS_DIRECTIVE_IN_FIELDS_ARGS,
            ErrorCode::DirectiveCompositionError => &DIRECTIVE_COMPOSITION_ERROR,
            ErrorCode::InterfaceObjectUsageError => &INTERFACE_OBJECT_USAGE_ERROR,
            ErrorCode::InterfaceKeyNotOnImplementation => &INTERFACE_KEY_NOT_ON_IMPLEMENTATION,
            ErrorCode::InterfaceKeyMissingImplementationType => {
                &INTERFACE_KEY_MISSING_IMPLEMENTATION_TYPE
            }
        }
    }
}

lazy_static! {
    static ref CODE_DEF_BY_CODE: IndexMap<String, &'static ErrorCodeDefinition> = ErrorCode::iter()
        .map(|e| {
            let def = e.definition();
            (def.code.clone(), def)
        })
        .collect();
}
