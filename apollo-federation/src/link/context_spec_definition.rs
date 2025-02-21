use std::sync::LazyLock;

use apollo_compiler::ast::Directive;
use apollo_compiler::ast::DirectiveDefinition;
use apollo_compiler::name;
use apollo_compiler::Name;
use apollo_compiler::Node;
use regex::Regex;

use crate::error::FederationError;
use crate::error::SingleFederationError;
use crate::internal_error;
use crate::link::argument::directive_required_string_argument;
use crate::link::spec::Identity;
use crate::link::spec::Url;
use crate::link::spec::Version;
use crate::link::spec_definition::SpecDefinition;
use crate::link::spec_definition::SpecDefinitions;
use crate::schema::FederationSchema;
use crate::utils::iter_into_single_item;

pub(crate) const CONTEXT_DIRECTIVE_NAME_IN_SPEC: Name = name!("context");
pub(crate) const CONTEXT_NAME_ARGUMENT_NAME: Name = name!("name");

static CONTEXT_PARSING_LEADING_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^(?:[\n\r\t ,]|#[^\n\r]*)*((?s:.)*)$"#).unwrap());

static CONTEXT_PARSING_CONTEXT_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^([A-Za-z_](?-u:\w)*)((?s:.)*)$"#).unwrap());

pub(crate) struct ContextDirectiveArguments<'doc> {
    pub(crate) name: &'doc str,
}

#[derive(Clone)]
pub(crate) struct ContextSpecDefinition {
    url: Url,
}

impl ContextSpecDefinition {
    pub(crate) fn new(version: Version) -> Self {
        Self {
            url: Url {
                identity: Identity::context_identity(),
                version,
            },
        }
    }

    pub(crate) fn context_directive_definition<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Result<&'schema Node<DirectiveDefinition>, FederationError> {
        self.directive_definition(schema, &CONTEXT_DIRECTIVE_NAME_IN_SPEC)?
            .ok_or_else(|| internal_error!("Unexpectedly could not find context spec in schema"))
    }

    pub(crate) fn context_directive_arguments<'doc>(
        &self,
        application: &'doc Node<Directive>,
    ) -> Result<ContextDirectiveArguments<'doc>, FederationError> {
        Ok(ContextDirectiveArguments {
            name: directive_required_string_argument(application, &CONTEXT_NAME_ARGUMENT_NAME)?,
        })
    }
}

impl SpecDefinition for ContextSpecDefinition {
    fn url(&self) -> &Url {
        &self.url
    }
}

pub(crate) static CONTEXT_VERSIONS: LazyLock<SpecDefinitions<ContextSpecDefinition>> =
    LazyLock::new(|| {
        let mut definitions = SpecDefinitions::new(Identity::context_identity());
        definitions.add(ContextSpecDefinition::new(Version { major: 0, minor: 1 }));
        definitions
    });

pub(crate) fn parse_context(field: &str) -> Result<(String, String), FederationError> {
    // PORT_NOTE: The original JS regex, as shown below
    //   /^(?:[\n\r\t ,]|#[^\n\r]*(?![^\n\r]))*\$(?:[\n\r\t ,]|#[^\n\r]*(?![^\n\r]))*([A-Za-z_]\w*(?!\w))([\s\S]*)$/
    // makes use of negative lookaheads, which aren't supported natively by Rust's regex crate.
    // There's a fancy_regex crate which does support this, but in the specific case above, the
    // negative lookaheads are just used to ensure strict *-greediness for the preceding expression
    // (i.e., it guarantees those *-expressions match greedily and won't backtrack).
    //
    // We can emulate that in this case by matching a series of regexes instead of a single regex,
    // where for each regex, the relevant *-expression doesn't backtrack by virtue of the rest of
    // the haystack guaranteeing a match. Also note that Rust has (?s:.) to match all characters
    // including newlines, which we use in place of JS's common regex workaround of [\s\S].
    fn strip_leading_ignored_tokens(input: &str) -> Result<&str, FederationError> {
        iter_into_single_item(CONTEXT_PARSING_LEADING_PATTERN.captures_iter(input))
            .and_then(|c| c.get(1))
            .map(|m| m.as_str())
            .ok_or_else(|| context_parse_error(input, "Failed to skip any leading ignored tokens"))
    }

    let dollar_start = strip_leading_ignored_tokens(field)?;
    let mut dollar_iter = dollar_start.chars();
    if dollar_iter.next() != Some('$') {
        return Err(context_parse_error(
            dollar_start,
            r#"Failed to find leading "$""#,
        ));
    }
    let after_dollar = dollar_iter.as_str();

    let context_start = strip_leading_ignored_tokens(after_dollar)?;
    let Some(context_captures) =
        iter_into_single_item(CONTEXT_PARSING_CONTEXT_PATTERN.captures_iter(context_start))
    else {
        return Err(context_parse_error(
            dollar_start,
            "Failed to find context name token and selection",
        ));
    };

    let context = match context_captures.get(1).map(|m| m.as_str()) {
        Some(context) if !context.is_empty() => context,
        _ => {
            return Err(context_parse_error(
                context_start,
                "Expected to find non-empty context name",
            ));
        }
    };

    let selection = match context_captures.get(2).map(|m| m.as_str()) {
        Some(selection) if !selection.is_empty() => selection,
        _ => {
            return Err(context_parse_error(
                context_start,
                "Expected to find non-empty selection",
            ));
        }
    };

    // PORT_NOTE: apollo_compiler's parsing code for field sets requires ignored tokens to be
    // pre-stripped if curly braces are missing, so we additionally do that here.
    let selection = strip_leading_ignored_tokens(selection)?;
    Ok((context.to_owned(), selection.to_owned()))
}

fn context_parse_error(context: &str, message: &str) -> FederationError {
    SingleFederationError::FromContextParseError {
        context: context.to_string(),
        message: message.to_string(),
    }
    .into()
}
