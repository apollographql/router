use std::collections::HashMap;
use std::fmt;
use std::str;
use std::sync::Arc;

use apollo_compiler::ast::Directive;
use apollo_compiler::ast::Value;
use apollo_compiler::name;
use apollo_compiler::InvalidNameError;
use apollo_compiler::Name;
use apollo_compiler::Node;
use thiserror::Error;

use crate::error::FederationError;
use crate::error::SingleFederationError;
use crate::link::link_spec_definition::LinkSpecDefinition;
use crate::link::link_spec_definition::CORE_VERSIONS;
use crate::link::link_spec_definition::LINK_VERSIONS;
use crate::link::spec::Identity;
use crate::link::spec::Url;

pub(crate) mod argument;
pub(crate) mod cost_spec_definition;
pub mod database;
pub(crate) mod federation_spec_definition;
pub(crate) mod graphql_definition;
pub(crate) mod inaccessible_spec_definition;
pub(crate) mod join_spec_definition;
pub(crate) mod link_spec_definition;
pub mod spec;
pub(crate) mod spec_definition;

pub const DEFAULT_LINK_NAME: Name = name!("link");
pub const DEFAULT_IMPORT_SCALAR_NAME: Name = name!("Import");
pub const DEFAULT_PURPOSE_ENUM_NAME: Name = name!("Purpose");

// TODO: we should provide proper "diagnostic" here, linking to ast, accumulating more than one
// error and whatnot.
#[derive(Error, Debug, PartialEq)]
pub enum LinkError {
    #[error(transparent)]
    InvalidName(#[from] InvalidNameError),
    #[error("Invalid use of @link in schema: {0}")]
    BootstrapError(String),
}

// TODO: Replace LinkError usages with FederationError.
impl From<LinkError> for FederationError {
    fn from(value: LinkError) -> Self {
        SingleFederationError::InvalidLinkDirectiveUsage {
            message: value.to_string(),
        }
        .into()
    }
}

#[derive(Eq, PartialEq, Debug)]
pub enum Purpose {
    SECURITY,
    EXECUTION,
}

impl Purpose {
    pub fn from_value(value: &Value) -> Result<Purpose, LinkError> {
        if let Value::Enum(value) = value {
            Ok(value.parse::<Purpose>()?)
        } else {
            Err(LinkError::BootstrapError(
                "invalid `purpose` value, should be an enum".to_string(),
            ))
        }
    }
}

impl str::FromStr for Purpose {
    type Err = LinkError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "SECURITY" => Ok(Purpose::SECURITY),
            "EXECUTION" => Ok(Purpose::EXECUTION),
            _ => Err(LinkError::BootstrapError(format!(
                "invalid/unrecognized `purpose` value '{}'",
                s
            ))),
        }
    }
}

impl fmt::Display for Purpose {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Purpose::SECURITY => f.write_str("SECURITY"),
            Purpose::EXECUTION => f.write_str("EXECUTION"),
        }
    }
}

impl From<&Purpose> for Name {
    fn from(value: &Purpose) -> Self {
        match value {
            Purpose::SECURITY => name!("SECURITY"),
            Purpose::EXECUTION => name!("EXECUTION"),
        }
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
    pub fn from_value(value: &Value) -> Result<Import, LinkError> {
        // TODO: it could be nice to include the broken value in the error messages of this method
        // (especially since @link(import:) is a list), but `Value` does not implement `Display`
        // currently, so a bit annoying.
        match value {
            Value::String(str) => {
                if let Some(directive_name) = str.strip_prefix('@') {
                    Ok(Import { element: Name::new(directive_name)?, is_directive: true, alias: None })
                } else {
                    Ok(Import { element: Name::new(str)?, is_directive: false, alias: None })
                }
            },
            Value::Object(fields) => {
                let mut name: Option<&str> = None;
                let mut alias: Option<&str> = None;
                for (k, v) in fields {
                    match k.as_str() {
                        "name" => {
                            name = Some(v.as_str().ok_or_else(|| {
                                LinkError::BootstrapError("invalid value for `name` field in @link(import:) argument: must be a string".to_string())
                            })?)
                        },
                        "as" => {
                            alias = Some(v.as_str().ok_or_else(|| {
                                LinkError::BootstrapError("invalid value for `as` field in @link(import:) argument: must be a string".to_string())
                            })?)
                        },
                        _ => Err(LinkError::BootstrapError(format!("unknown field `{k}` in @link(import:) argument")))?
                    }
                }
                if let Some(element) = name {
                    if let Some(directive_name) = element.strip_prefix('@') {
                        if let Some(alias_str) = alias.as_ref() {
                            let Some(alias_str) = alias_str.strip_prefix('@') else {
                                return Err(LinkError::BootstrapError(format!("invalid alias '{}' for import name '{}': should start with '@' since the imported name does", alias_str, element)));
                            };
                            alias = Some(alias_str);
                        }
                        Ok(Import {
                            element: Name::new(directive_name)?,
                            is_directive: true,
                            alias: alias.map(Name::new).transpose()?,
                        })
                    } else {
                        if let Some(alias) = &alias {
                            if alias.starts_with('@') {
                                return Err(LinkError::BootstrapError(format!("invalid alias '{}' for import name '{}': should not start with '@' (or, if {} is a directive, then the name should start with '@')", alias, element, element)));
                            }
                        }
                        Ok(Import {
                            element: Name::new(element)?,
                            is_directive: false,
                            alias: alias.map(Name::new).transpose()?,
                        })
                    }
                } else {
                    Err(LinkError::BootstrapError("invalid entry in @link(import:) argument, missing mandatory `name` field".to_string()))
                }
            },
            _ => Err(LinkError::BootstrapError("invalid sub-value for @link(import:) argument: values should be either strings or input object values of the form { name: \"<importedElement>\", as: \"<alias>\" }.".to_string()))
        }
    }

    pub fn element_display_name(&self) -> impl fmt::Display + '_ {
        DisplayName {
            name: &self.element,
            is_directive: self.is_directive,
        }
    }

    pub fn imported_name(&self) -> &Name {
        return self.alias.as_ref().unwrap_or(&self.element);
    }

    pub fn imported_display_name(&self) -> impl fmt::Display + '_ {
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

impl<'s> fmt::Display for DisplayName<'s> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_directive {
            f.write_str("@")?;
        }
        f.write_str(self.name)
    }
}

impl fmt::Display for Import {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.alias.is_some() {
            write!(
                f,
                r#"{{ name: "{}", as: "{}" }}"#,
                self.element_display_name(),
                self.imported_display_name()
            )
        } else {
            write!(f, r#""{}""#, self.imported_display_name())
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct Link {
    pub url: Url,
    pub spec_alias: Option<Name>,
    pub imports: Vec<Arc<Import>>,
    pub purpose: Option<Purpose>,
}

impl Link {
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

    pub fn from_directive_application(directive: &Node<Directive>) -> Result<Link, LinkError> {
        let (url, is_link) = if let Some(value) = directive.argument_by_name("url") {
            (value, true)
        } else if let Some(value) = directive.argument_by_name("feature") {
            // XXX(@goto-bus-stop): @core compatibility is primarily to support old tests--should be
            // removed when those are updated.
            (value, false)
        } else {
            return Err(LinkError::BootstrapError(
                "the `url` argument for @link is mandatory".to_string(),
            ));
        };

        let (directive_name, arg_name) = if is_link {
            ("link", "url")
        } else {
            ("core", "feature")
        };

        let url = url.as_str().ok_or_else(|| {
            LinkError::BootstrapError(format!(
                "the `{arg_name}` argument for @{directive_name} must be a String"
            ))
        })?;
        let url: Url = url.parse::<Url>().map_err(|e| {
            LinkError::BootstrapError(format!("invalid `{arg_name}` argument (reason: {e})"))
        })?;

        let spec_alias = directive
            .argument_by_name("as")
            .and_then(|arg| arg.as_str())
            .map(Name::new)
            .transpose()?;
        let purpose = if let Some(value) = directive.argument_by_name("for") {
            Some(Purpose::from_value(value)?)
        } else {
            None
        };

        let imports = if is_link {
            directive
                .argument_by_name("import")
                .and_then(|arg| arg.as_list())
                .unwrap_or(&[])
                .iter()
                .map(|value| Ok(Arc::new(Import::from_value(value)?)))
                .collect::<Result<Vec<Arc<Import>>, LinkError>>()?
        } else {
            Default::default()
        };

        Ok(Link {
            url,
            spec_alias,
            imports,
            purpose,
        })
    }
}

impl fmt::Display for Link {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let imported_types: Vec<String> = self
            .imports
            .iter()
            .map(|import| import.to_string())
            .collect::<Vec<String>>();
        let imports = if imported_types.is_empty() {
            "".to_string()
        } else {
            format!(r#", import: [{}]"#, imported_types.join(", "))
        };
        let alias = self
            .spec_alias
            .as_ref()
            .map(|a| format!(r#", as: "{}""#, a))
            .unwrap_or("".to_string());
        let purpose = self
            .purpose
            .as_ref()
            .map(|p| format!(r#", for: {}"#, p))
            .unwrap_or("".to_string());
        write!(f, r#"@link(url: "{}"{alias}{imports}{purpose})"#, self.url)
    }
}

#[derive(Debug)]
pub struct LinkedElement {
    pub link: Arc<Link>,
    pub import: Option<Arc<Import>>,
}

#[derive(Default, Eq, PartialEq, Debug)]
pub struct LinksMetadata {
    pub(crate) links: Vec<Arc<Link>>,
    pub(crate) by_identity: HashMap<Identity, Arc<Link>>,
    pub(crate) by_name_in_schema: HashMap<Name, Arc<Link>>,
    pub(crate) types_by_imported_name: HashMap<Name, (Arc<Link>, Arc<Import>)>,
    pub(crate) directives_by_imported_name: HashMap<Name, (Arc<Link>, Arc<Import>)>,
}

impl LinksMetadata {
    pub(crate) fn link_spec_definition(
        &self,
    ) -> Result<&'static LinkSpecDefinition, FederationError> {
        if let Some(link_link) = self.for_identity(&Identity::link_identity()) {
            LINK_VERSIONS.find(&link_link.url.version).ok_or_else(|| {
                SingleFederationError::Internal {
                    message: format!("Unexpected link spec version {}", link_link.url.version),
                }
                .into()
            })
        } else if let Some(core_link) = self.for_identity(&Identity::core_identity()) {
            CORE_VERSIONS.find(&core_link.url.version).ok_or_else(|| {
                SingleFederationError::Internal {
                    message: format!("Unexpected core spec version {}", core_link.url.version),
                }
                .into()
            })
        } else {
            Err(SingleFederationError::Internal {
                message: "Unexpectedly could not find core/link spec".to_owned(),
            }
            .into())
        }
    }

    pub fn all_links(&self) -> &[Arc<Link>] {
        return self.links.as_ref();
    }

    pub fn for_identity(&self, identity: &Identity) -> Option<Arc<Link>> {
        return self.by_identity.get(identity).cloned();
    }

    pub fn source_link_of_type(&self, type_name: &Name) -> Option<LinkedElement> {
        // For types, it's either an imported name or it must be fully qualified

        if let Some((link, import)) = self.types_by_imported_name.get(type_name) {
            Some(LinkedElement {
                link: Arc::clone(link),
                import: Some(Arc::clone(import)),
            })
        } else {
            type_name.split_once("__").and_then(|(spec_name, _)| {
                self.by_name_in_schema
                    .get(spec_name)
                    .map(|link| LinkedElement {
                        link: Arc::clone(link),
                        import: None,
                    })
            })
        }
    }

    pub fn source_link_of_directive(&self, directive_name: &Name) -> Option<LinkedElement> {
        // For directives, it can be either:
        //   1. be an imported name,
        //   2. be the "imported" name of a linked spec (special case of a directive named like the
        //      spec),
        //   3. or it must be fully qualified.
        if let Some((link, import)) = self.directives_by_imported_name.get(directive_name) {
            return Some(LinkedElement {
                link: Arc::clone(link),
                import: Some(Arc::clone(import)),
            });
        }

        if let Some(link) = self.by_name_in_schema.get(directive_name) {
            return Some(LinkedElement {
                link: Arc::clone(link),
                import: None,
            });
        }

        directive_name.split_once("__").and_then(|(spec_name, _)| {
            self.by_name_in_schema
                .get(spec_name)
                .map(|link| LinkedElement {
                    link: Arc::clone(link),
                    import: None,
                })
        })
    }
}
