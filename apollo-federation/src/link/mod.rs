use std::fmt;
use std::ops::Range;
use std::sync::Arc;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use apollo_compiler::ast::Directive;
use apollo_compiler::ast::Value;
use apollo_compiler::parser::LineColumn;
use apollo_compiler::schema::Component;

use crate::error::FederationError;
use crate::link::link_spec_definition::CORE_VERSIONS;
use crate::link::link_spec_definition::LINK_VERSIONS;
use crate::link::spec::Identity;
use crate::link::spec::Url;

pub(crate) mod argument;
pub(crate) mod authenticated_spec_definition;
pub(crate) mod cache_tag_spec_definition;
pub(crate) mod context_spec_definition;
pub mod cost_spec_definition;
pub(crate) mod federation_spec_definition;
pub(crate) mod graphql_definition;
pub(crate) mod inaccessible_spec_definition;
pub(crate) mod join_spec_definition;
pub(crate) mod link_spec_definition;
pub mod metadata;
pub(crate) mod policy_spec_definition;
pub(crate) mod requires_scopes_spec_definition;
pub mod spec;
pub(crate) mod spec_definition;
pub(crate) mod spec_registry;
pub(crate) mod tag_spec_definition;

#[derive(Clone, Copy, Eq, PartialEq, Debug)]
pub enum Purpose {
    SECURITY,
    EXECUTION,
}

impl fmt::Display for Purpose {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        Value::from(self).fmt(f)
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Import {
    /// The name of the element that is being imported.
    ///
    /// Note that this will never start with '@': whether or not this is the name of a directive is
    /// entirely reflected by the value of `is_directive`.
    pub element: Name,

    /// Whether the imported element is a directive (if it is not, then it is an imported type).
    pub is_directive: bool,

    /// The optional alias under which the element is imported.
    pub alias: Option<Name>,
}

impl Import {
    pub fn name_in_schema(&self) -> &Name {
        self.alias.as_ref().unwrap_or(&self.element)
    }

    pub fn element_name_in_spec(&self) -> ElementName {
        ElementName {
            name: self.element.clone(),
            is_directive: self.is_directive,
        }
    }

    pub fn element_name_in_schema(&self) -> ElementName {
        ElementName {
            name: self.name_in_schema().clone(),
            is_directive: self.is_directive,
        }
    }
}

/// The name of a type or directive, regardless of whether it's a name-in-spec or a name-in-schema.
/// Note that this is cheap to clone since [Name] is cheap to clone.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ElementName {
    pub name: Name,
    pub is_directive: bool,
}

impl fmt::Display for ElementName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_directive {
            f.write_str("@")?;
        }
        f.write_str(&self.name)
    }
}

impl fmt::Display for Import {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Value::from(self).fmt(f)
    }
}

/// Metadata about a single application of @link in a schema.
// PORT_NOTE: Named `CoreFeature` in the JS codebase, but "core" is outdated terminology.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Link {
    pub url: Url,
    pub spec_alias: Option<Name>,
    pub imports: Vec<Arc<Import>>,
    pub purpose: Option<Purpose>,
    pub line_column_range: Option<Range<LineColumn>>,
}

impl Link {
    /// Use [super::LinkSpecDefinition::link_from_directive] instead of this method where possible,
    /// since this method guesses the version of the link/core spec.
    pub(crate) fn from_directive_application_when_link_spec_unknown(
        directive: &Node<Directive>,
        schema: &Schema,
    ) -> Result<Link, FederationError> {
        LINK_VERSIONS
            .latest()
            .link_from_directive(directive, schema)
            .or_else(|error| {
                // If the directive couldn't be parsed as a @link application, try parsing it as @core
                // one, though if that also errors then prefer the original @link-parsing errors.
                CORE_VERSIONS
                    .latest()
                    .link_from_directive(directive, schema)
                    .or(Err(error))
            })
    }

    pub fn spec_name_in_schema(&self) -> Name {
        if let Some(spec_alias) = &self.spec_alias {
            return spec_alias.clone();
        }
        let name = &self.url.identity.name;
        name.clone().try_into().unwrap_or_else(|_| {
            // TODO: While @link does allow for specs with invalid names as long as they have valid
            // aliases, for backwards compatibility, we need to support @link applications that have
            // an invalid name and no alias. These have historically been fine because such schemas
            // do not use the default names for types/directives from the spec being linked. So
            // ideally, we would return a `Result::Err` in `directive_name_in_schema()` and
            // `type_name_in_schema()` when someone tries to compute an element name-in-schema that
            // would be an invalid name. However, that would cause many upstream callers to also
            // have to return `Result`. For now, we're leaving that step to the future. (We wouldn't
            // do that in this method because it's used in `LinksMetadata::add_link()`.)
            Name::new_unchecked(name)
        })
    }

    pub fn directive_name_in_schema(&self, name: &Name) -> Name {
        // If the directive is imported, then it's name in schema is whatever name it is
        // imported under. Otherwise, it is usually fully qualified by the spec name (so,
        // something like 'federation__key'), but there is a special case for directives
        // whose name match the one of the spec: those don't get qualified.
        if let Some(import) = self.imports.iter().find(|i| i.element == *name) {
            import.alias.clone().unwrap_or_else(|| name.clone())
        } else if name.as_str() == self.url.identity.name.as_ref() {
            self.spec_name_in_schema().clone()
        } else {
            // Both sides are `Name`s and we just add valid characters in between.
            Name::new_unchecked(&format!("{}__{}", self.spec_name_in_schema(), name))
        }
    }

    pub(crate) fn directive_name_in_schema_for_core_arguments(
        spec_url: &Url,
        spec_name_in_schema: &Name,
        imports: &[Import],
        directive_name_in_spec: &Name,
    ) -> Name {
        if let Some(element_import) = imports
            .iter()
            .find(|i| i.element == *directive_name_in_spec)
        {
            element_import.name_in_schema().clone()
        } else if spec_url.identity.name.as_ref() == directive_name_in_spec.as_str() {
            spec_name_in_schema.clone()
        } else {
            Name::new_unchecked(format!("{spec_name_in_schema}__{directive_name_in_spec}").as_str())
        }
    }

    pub fn type_name_in_schema(&self, name: &Name) -> Name {
        // Similar to directives, but the special case of a directive name matching the spec
        // name does not apply to types.
        if let Some(import) = self.imports.iter().find(|i| i.element == *name) {
            import.alias.clone().unwrap_or_else(|| name.clone())
        } else {
            // Both sides are `Name`s and we just add valid characters in between.
            Name::new_unchecked(&format!("{}__{}", self.spec_name_in_schema(), name))
        }
    }

    pub(crate) fn for_identity<'schema>(
        schema: &'schema Schema,
        identity: &Identity,
    ) -> Option<(Self, &'schema Component<Directive>)> {
        schema
            .schema_definition
            .directives
            .iter()
            .find_map(|directive| {
                let link =
                    Link::from_directive_application_when_link_spec_unknown(directive, schema)
                        .ok()?;
                if link.url.identity == *identity {
                    Some((link, directive))
                } else {
                    None
                }
            })
    }
}

impl fmt::Display for Link {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        LINK_VERSIONS.latest().directive_from_link(self).fmt(f)
    }
}
