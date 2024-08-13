use std::collections::HashSet;

use super::JSONSelection;
use super::NamedSelection;
use super::PathSelection;
use super::SubSelection;

/// A representation of a static parameter.
///
/// Each parameter can include path components for drilling down into specific
/// members of a parameter.
///
/// Note: This is somewhat related to [apollo_federation::sources::connect::url_template::Parameter]
/// but is less restrictive as it does not do any formal validation of parameters.
///
/// e.g. A parameter like below
/// ```json_selection
/// $this.a.b.c
/// ```
///
/// would have the following representation:
/// ```rust
/// # use apollo_federation::sources::connect::StaticParameter;
/// StaticParameter {
///   name: "this",
///   paths: vec!["a", "b", "c"],
/// }
/// # ;
/// ```
#[derive(Debug, Hash, PartialEq, Eq)]
pub struct StaticParameter<'a> {
    /// The name of the parameter, after the $
    /// TODO: This might be nice to have as an enum, but it requires making
    /// extraction fallible. Another option would be to have JSONSelection aware
    /// of which variables it knows about, but that might not make sense to have
    /// as a responsibility of JSONSelection.
    pub name: &'a str,

    /// Any paths after the name
    pub paths: Vec<&'a str>,
}

pub trait ExtractParameters {
    /// Extract parameters for static analysis
    fn extract_parameters(&self) -> Option<HashSet<StaticParameter>>;
}

impl ExtractParameters for JSONSelection {
    fn extract_parameters(&self) -> Option<HashSet<StaticParameter>> {
        match &self {
            JSONSelection::Named(named) => named.extract_parameters(),
            JSONSelection::Path(path) => path.extract_parameters(),
        }
    }
}

impl ExtractParameters for PathSelection {
    fn extract_parameters(&self) -> Option<HashSet<StaticParameter>> {
        let param = match &self {
            PathSelection::Var(name, rest) => Some(StaticParameter {
                name: name.as_str(),
                paths: rest
                    .collect_paths()
                    .iter()
                    // We don't run `to_string` here since path implements display and prepends
                    // a '.' to the path components
                    .map(|k| match k {
                        super::Key::Field(val) | super::Key::Quoted(val) => val.as_str(),
                        super::Key::Index(_) => "[]", // TODO: Remove when JSONSelection removes it
                    })
                    .collect(),
            }),
            PathSelection::Key(_, _) | PathSelection::Selection(_) | PathSelection::Empty => None,
        };

        param.map(|p| {
            let mut set = HashSet::with_hasher(Default::default());
            set.insert(p);

            set
        })
    }
}

impl ExtractParameters for SubSelection {
    fn extract_parameters(&self) -> Option<HashSet<StaticParameter>> {
        let params: HashSet<_> = self
            .selections
            .iter()
            .filter_map(NamedSelection::extract_parameters)
            .flatten()
            .collect();

        if params.is_empty() {
            None
        } else {
            Some(params)
        }
    }
}

impl ExtractParameters for NamedSelection {
    fn extract_parameters(&self) -> Option<HashSet<StaticParameter>> {
        match &self {
            NamedSelection::Field(_, _, Some(sub))
            | NamedSelection::Quoted(_, _, Some(sub))
            | NamedSelection::Group(_, sub) => sub.extract_parameters(),

            NamedSelection::Path(_, path) => path.extract_parameters(),
            _ => None,
        }
    }
}
