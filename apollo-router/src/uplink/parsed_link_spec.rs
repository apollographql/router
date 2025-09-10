use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast;
use apollo_compiler::schema::Directive;
use url::Url;

use crate::spec::LINK_AS_ARGUMENT;
use crate::spec::LINK_URL_ARGUMENT;

#[derive(Debug)]
pub(super) struct ParsedLinkSpec {
    pub(super) spec_name: String,
    pub(super) version: semver::Version,
    pub(super) spec_url: String,
    pub(super) imported_as: Option<String>,
    pub(super) url: String,
}

impl ParsedLinkSpec {
    pub(super) fn from_link_directive(
        link_directive: &Directive,
    ) -> Option<Result<ParsedLinkSpec, url::ParseError>> {
        link_directive
            .specified_argument_by_name(LINK_URL_ARGUMENT)
            .and_then(|value| {
                let url_string = value.as_str();

                let parsed_url = Url::parse(url_string.unwrap_or_default()).ok()?;

                let mut segments = parsed_url.path_segments()?;
                let spec_name = segments.next()?.to_string();
                let spec_url = format!(
                    "{}://{}/{}",
                    parsed_url.scheme(),
                    parsed_url.host()?,
                    spec_name
                );
                let version_string = segments.next()?.strip_prefix('v')?;
                let parsed_version =
                    semver::Version::parse(format!("{}.0", &version_string).as_str()).ok()?;

                let imported_as = link_directive
                    .specified_argument_by_name(LINK_AS_ARGUMENT)
                    .map(|as_arg| as_arg.as_str().unwrap_or_default().to_string());

                Some(Ok(ParsedLinkSpec {
                    spec_name,
                    spec_url,
                    version: parsed_version,
                    imported_as,
                    url: url_string?.to_string(),
                }))
            })
    }

    pub(super) fn from_join_directive_args(
        args: &[(Name, Node<ast::Value>)],
    ) -> Option<Result<Self, url::ParseError>> {
        let url_string = args
            .iter()
            .find(|(name, _)| name == &Name::new_unchecked(LINK_URL_ARGUMENT))
            .and_then(|(_, value)| value.as_str());

        let parsed_url = Url::parse(url_string.unwrap_or_default()).ok()?;

        let mut segments = parsed_url.path_segments()?;
        let spec_name = segments.next()?.to_string();
        let spec_url = format!(
            "{}://{}/{}",
            parsed_url.scheme(),
            parsed_url.host()?,
            spec_name
        );
        let version_string = segments.next()?.strip_prefix('v')?;
        let parsed_version =
            semver::Version::parse(format!("{}.0", &version_string).as_str()).ok()?;

        let imported_as = args
            .iter()
            .find(|(name, _)| name == &Name::new_unchecked(LINK_AS_ARGUMENT))
            .and_then(|(_, value)| value.as_str())
            .map(|s| s.to_string());

        Some(Ok(ParsedLinkSpec {
            spec_name,
            spec_url,
            version: parsed_version,
            imported_as,
            url: url_string?.to_string(),
        }))
    }

    // Implements directive name construction logic for link directives.
    // 1. If the link directive has an `as` argument, use that as the prefix.
    // 2. If the link directive's spec name is the same as the default name, use the default name with no prefix.
    // 3. Otherwise, use the spec name as the prefix.
    pub(super) fn directive_name(&self, default_name: &str) -> String {
        if let Some(imported_as) = &self.imported_as {
            format!("{imported_as}__{default_name}")
        } else if self.spec_name == default_name {
            default_name.to_string()
        } else {
            format!("{}__{}", self.spec_name, default_name)
        }
    }
}
