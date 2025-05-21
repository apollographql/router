//! Validations for `@link(url: "https://specs.apollo.dev/connect")`

use std::fmt::Display;

use apollo_compiler::Name;
use apollo_compiler::Schema;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::Directive;
use itertools::Itertools;
use strum::IntoEnumIterator;

use crate::link::Link;
use crate::sources::connect::ConnectSpec;
use crate::sources::connect::validation::Code;
use crate::sources::connect::validation::Message;

/// The `@link` in a subgraph which enables connectors
#[derive(Clone, Debug)]
pub(crate) struct ConnectLink<'schema> {
    spec: ConnectSpec,
    source_directive_name: Name,
    connect_directive_name: Name,
    directive: &'schema Component<Directive>,
    link: Link,
}

impl<'schema> ConnectLink<'schema> {
    /// Find the connect link, if any, and validate it.
    /// Returns `None` if this is not a connectors subgraph.
    ///
    /// # Errors
    /// - Unknown spec version
    pub(super) fn new(schema: &'schema Schema) -> Option<Result<Self, Message>> {
        let connect_identity = ConnectSpec::identity();
        let (link, directive) = Link::for_identity(schema, &connect_identity)?;

        let spec = match ConnectSpec::try_from(&link.url.version) {
            Err(err) => {
                let message = format!(
                    "{err}; should be one of {available_versions}.",
                    available_versions = ConnectSpec::iter().map(ConnectSpec::as_str).join(", "),
                );
                return Some(Err(Message {
                    code: Code::UnknownConnectorsVersion,
                    message,
                    locations: directive
                        .line_column_range(&schema.sources)
                        .into_iter()
                        .collect(),
                }));
            }
            Ok(spec) => spec,
        };
        let source_directive_name = ConnectSpec::source_directive_name(&link);
        let connect_directive_name = ConnectSpec::connect_directive_name(&link);
        Some(Ok(Self {
            spec,
            source_directive_name,
            connect_directive_name,
            directive,
            link,
        }))
    }

    pub(super) fn spec(&self) -> ConnectSpec {
        self.spec
    }

    pub(super) fn source_directive_name(&self) -> &Name {
        &self.source_directive_name
    }

    pub(super) fn connect_directive_name(&self) -> &Name {
        &self.connect_directive_name
    }

    pub(super) fn directive(&self) -> &Component<Directive> {
        self.directive
    }
}

impl Display for ConnectLink<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.link)
    }
}
