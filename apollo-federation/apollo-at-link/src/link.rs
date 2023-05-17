use crate::spec::Identity;
use crate::spec::Url;
use apollo_compiler::ast::{Directive, Value};
use std::fmt;
use std::str;
use std::{collections::HashMap, sync::Arc};
use thiserror::Error;

pub const DEFAULT_LINK_NAME: &str = "link";
pub const DEFAULT_IMPORT_SCALAR_NAME: &str = "Import";
pub const DEFAULT_PURPOSE_ENUM_NAME: &str = "Purpose";

// TODO: we should provide proper "diagnostic" here, linking to ast, accumulating more than one
// error and whatnot.
#[derive(Error, Debug, PartialEq)]
pub enum LinkError {
    #[error("Invalid use of @link in schema: {0}")]
    BootstrapError(String),
}

#[derive(Eq, PartialEq, Debug)]
pub enum Purpose {
    SECURITY,
    EXECUTION,
}

impl Purpose {
    pub fn from_ast_value(value: &Value) -> Result<Purpose, LinkError> {
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
        let str = match self {
            Purpose::SECURITY => "SECURITY",
            Purpose::EXECUTION => "EXECUTION",
        };
        write!(f, "{}", str)
    }
}

#[derive(Eq, PartialEq, Debug)]
pub struct Import {
    /// The name of the element that is being imported.
    ///
    /// Note that this will never start with '@': whether or not this is the name of a directive is
    /// entirely reflected by the value of `is_directive`.
    pub element: String,

    /// Whether the imported element is a directive (if it is not, then it is an imported type).
    pub is_directive: bool,

    /// The optional alias under which the element is imported.
    pub alias: Option<String>,
}

impl Import {
    pub fn from_hir_value(value: &Value) -> Result<Import, LinkError> {
        // TODO: it could be nice to include the broken value in the error messages of this method
        // (especially since @link(import:) is a list), but `Value` does not implement `Display`
        // currently, so a bit annoying.
        match value {
            Value::String(str) => {
                let is_directive = str.starts_with('@');
                let element = if is_directive {
                    str.strip_prefix('@').unwrap().to_string()
                } else {
                    str.to_string()
                };
                Ok(Import { element, is_directive, alias: None })
            },
            Value::Object(fields) => {
                let mut name: Option<String> = None;
                let mut alias: Option<String> = None;
                for (k, v) in fields {
                    match k.as_str() {
                        "name" => {
                            name = Some(v.as_str().ok_or_else(|| {
                                LinkError::BootstrapError("invalid value for `name` field in @link(import:) argument: must be a string".to_string())
                            })?.to_owned())
                        },
                        "as" => {
                            alias = Some(v.as_str().ok_or_else(|| {
                                LinkError::BootstrapError("invalid value for `as` field in @link(import:) argument: must be a string".to_string())
                            })?.to_owned())
                        },
                        _ => Err(LinkError::BootstrapError(format!("unknown field `{k}` in @link(import:) argument")))?
                    }
                }
                if let Some(element) = name {
                    let is_directive = element.starts_with('@');
                    if is_directive {
                        let element = element.strip_prefix('@').unwrap().to_string();
                        if let Some(alias_str) = alias {
                            if !alias_str.starts_with('@') {
                                Err(LinkError::BootstrapError(format!("invalid alias '{}' for import name '{}': should start with '@' since the imported name does", alias_str, element)))?
                            }
                            alias = Some(alias_str.strip_prefix('@').unwrap().to_string());
                        }
                        Ok(Import { element, is_directive, alias })
                    } else {
                        if let Some(alias) = &alias {
                            if alias.starts_with('@') {
                                Err(LinkError::BootstrapError(format!("invalid alias '{}' for import name '{}': should not start with '@' (or, if {} is a directive, then the name should start with '@')", alias, element, element)))?
                            }
                        }
                        Ok(Import { element, is_directive, alias })
                    }
                } else {
                    Err(LinkError::BootstrapError("invalid entry in @link(import:) argument, missing mandatory `name` field".to_string()))
                }
            },
            _ => Err(LinkError::BootstrapError("invalid sub-value for @link(import:) argument: values should be either strings or input object values of the form { name: \"<importedElement>\", as: \"<alias>\" }.".to_string()))
        }
    }

    pub fn imported_name(&self) -> &String {
        return self.alias.as_ref().unwrap_or(&self.element);
    }

    pub fn imported_display_name(&self) -> String {
        if self.is_directive {
            format!("@{}", self.imported_name())
        } else {
            self.imported_name().clone()
        }
    }
}

impl fmt::Display for Import {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.alias.is_some() {
            write!(
                f,
                r#"{{ name: "{}", as: "{}" }}"#,
                if self.is_directive {
                    format!("@{}", self.element)
                } else {
                    self.element.clone()
                },
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
    pub spec_alias: Option<String>,
    pub imports: Vec<Arc<Import>>,
    pub purpose: Option<Purpose>,
}

impl Link {
    pub fn spec_name_in_schema(&self) -> &String {
        self.spec_alias.as_ref().unwrap_or(&self.url.identity.name)
    }

    pub fn directive_name_in_schema(&self, name: &str) -> String {
        // If the directive is imported, then it's name in schema is whatever name it is
        // imported under. Otherwise, it is usually fully qualified by the spec name (so,
        // something like 'federation__key'), but there is a special case for directives
        // whose name match the one of the spec: those don't get qualified.
        if let Some(import) = self.imports.iter().find(|i| i.element == name) {
            import.alias.clone().unwrap_or(name.to_string())
        } else if name == self.url.identity.name {
            self.spec_name_in_schema().clone()
        } else {
            format!("{}__{}", self.spec_name_in_schema(), name)
        }
    }

    pub fn type_name_in_schema(&self, name: &str) -> String {
        // Similar to directives, but the special case of a directive name matching the spec
        // name does not apply to types.
        if let Some(import) = self.imports.iter().find(|i| i.element == name) {
            import.alias.clone().unwrap_or(name.to_string())
        } else {
            format!("{}__{}", self.spec_name_in_schema(), name)
        }
    }

    pub fn from_directive_application(directive: &Directive) -> Result<Link, LinkError> {
        let url = directive
            .argument_by_name("url")
            .and_then(|arg| arg.as_str())
            .ok_or(LinkError::BootstrapError(
                "the `url` argument for @link is mandatory".to_string(),
            ))?;
        let url: Url = url.parse::<Url>().map_err(|e| {
            LinkError::BootstrapError(format!("invalid `url` argument (reason: {})", e))
        })?;
        let spec_alias = directive
            .argument_by_name("as")
            .and_then(|arg| arg.as_str())
            .map(|s| s.to_owned());
        let purpose = if let Some(value) = directive.argument_by_name("for") {
            Some(Purpose::from_ast_value(value)?)
        } else {
            None
        };
        let mut imports = Vec::new();
        if let Some(values) = directive
            .argument_by_name("import")
            .and_then(|arg| arg.as_list())
        {
            for v in values {
                imports.push(Arc::new(Import::from_hir_value(v)?));
            }
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
    pub(crate) by_name_in_schema: HashMap<String, Arc<Link>>,
    pub(crate) types_by_imported_name: HashMap<String, (Arc<Link>, Arc<Import>)>,
    pub(crate) directives_by_imported_name: HashMap<String, (Arc<Link>, Arc<Import>)>,
}

impl LinksMetadata {
    pub fn all_links(&self) -> &[Arc<Link>] {
        return self.links.as_ref();
    }

    pub fn for_identity(&self, identity: &Identity) -> Option<Arc<Link>> {
        return self.by_identity.get(identity).cloned();
    }

    pub fn source_link_of_type(&self, type_name: &str) -> Option<LinkedElement> {
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

    pub fn source_link_of_directive(&self, directive_name: &str) -> Option<LinkedElement> {
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
