use std::fmt;
use std::ops::Range;
use std::str;
use std::sync::Arc;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use apollo_compiler::ast::Directive;
use apollo_compiler::ast::Value;
use apollo_compiler::parser::LineColumn;
use apollo_compiler::schema::Component;

use crate::error::FederationError;
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

#[derive(Eq, PartialEq, Debug)]
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
    pub fn imported_name(&self) -> &Name {
        self.alias.as_ref().unwrap_or(&self.element)
    }

    pub fn element_display_name(&self) -> impl fmt::Display {
        DisplayName {
            name: &self.element,
            is_directive: self.is_directive,
        }
    }

    pub fn imported_display_name(&self) -> impl fmt::Display {
        DisplayName {
            name: self.imported_name(),
            is_directive: self.is_directive,
        }
    }
}

/// A [`fmt::Display`]able wrapper for name strings that adds an `@` in front for directive names.
struct DisplayName<'s> {
    name: &'s str,
    is_directive: bool,
}

impl fmt::Display for DisplayName<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_directive {
            f.write_str("@")?;
        }
        f.write_str(self.name)
    }
}

impl fmt::Display for Import {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Value::from(self).fmt(f)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Link {
    pub url: Url,
    pub spec_alias: Option<Name>,
    pub imports: Vec<Arc<Import>>,
    pub purpose: Option<Purpose>,
    pub line_column_range: Option<Range<LineColumn>>,
}

impl Link {
    pub fn from_directive_application(
        directive: &Node<Directive>,
        schema: &Schema,
    ) -> Result<Link, FederationError> {
        let mut link = Link::try_from(directive.as_ref())?;
        link.line_column_range = directive.line_column_range(&schema.sources);
        Ok(link)
    }

    pub fn spec_name_in_schema(&self) -> &Name {
        self.spec_alias.as_ref().unwrap_or(&self.url.identity.name)
    }

    pub fn directive_name_in_schema(&self, name: &Name) -> Name {
        // If the directive is imported, then it's name in schema is whatever name it is
        // imported under. Otherwise, it is usually fully qualified by the spec name (so,
        // something like 'federation__key'), but there is a special case for directives
        // whose name match the one of the spec: those don't get qualified.
        if let Some(import) = self.imports.iter().find(|i| i.element == *name) {
            import.alias.clone().unwrap_or_else(|| name.clone())
        } else if name == self.url.identity.name.as_str() {
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
            element_import.imported_name().clone()
        } else if spec_url.identity.name == *directive_name_in_spec {
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

    pub fn for_identity<'schema>(
        schema: &'schema Schema,
        identity: &Identity,
    ) -> Option<(Self, &'schema Component<Directive>)> {
        schema
            .schema_definition
            .directives
            .iter()
            .find_map(|directive| {
                let link = Link::from_directive_application(directive, schema).ok()?;
                if link.url.identity == *identity {
                    Some((link, directive))
                } else {
                    None
                }
            })
    }

    /// Returns true if this link has an import assigning an alias to the given element.
    pub(crate) fn renames(&self, element: &Name) -> bool {
        self.imports
            .iter()
            .find(|import| &import.element == element)
            .is_some_and(|import| *import.imported_name() != *element)
    }
}

impl fmt::Display for Link {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Directive::from(self).fmt(f)
    }
}
