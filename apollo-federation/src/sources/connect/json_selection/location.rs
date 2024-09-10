use nom::bytes::complete::tag;
use nom::combinator::map;
use nom::IResult;
use nom_locate::LocatedSpan;

pub type Span<'a> = LocatedSpan<&'a str>;

// Some parsed AST structures, like PathSelection and NamedSelection, can
// produce a range directly from their children, so they do not need to be
// wrapped as Parsed<PathSelection> or Parsed<NamedSelection>. Instead, these
// types implement the Ranged<T> trait for compatibility with the other
// Parsed<T> structures.
pub trait Ranged<T> {
    fn node(&self) -> &T;
    fn range(&self) -> Option<(usize, usize)>;
}

// The most common implementation of the Ranged<T> trait is the Parsed<T>
// struct, used for any AST node that needs its own location information
// (because that information is not derivable from its children).
#[derive(Debug, PartialEq, Eq, Clone, Hash)]
pub struct Parsed<T> {
    node: Box<T>,
    range: Option<(usize, usize)>,
}

pub(super) fn merge_ranges(
    left: Option<(usize, usize)>,
    right: Option<(usize, usize)>,
) -> Option<(usize, usize)> {
    match (left, right) {
        (Some((left_start, _left_end)), Some((_right_start, right_end))) => {
            Some((left_start, right_end))
        }
        (Some(left_range), None) => Some(left_range),
        (None, Some(right_range)) => Some(right_range),
        (None, None) => None,
    }
}

impl<T> std::ops::Deref for Parsed<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.node.as_ref()
    }
}
impl<T> std::ops::DerefMut for Parsed<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.node.as_mut()
    }
}

impl<T> AsRef<T> for Parsed<T> {
    fn as_ref(&self) -> &T {
        self.node.as_ref()
    }
}

impl<T> AsMut<T> for Parsed<T> {
    fn as_mut(&mut self) -> &mut T {
        self.node.as_mut()
    }
}

impl<T> PartialEq<T> for Parsed<T>
where
    T: PartialEq,
{
    fn eq(&self, other: &T) -> bool {
        self.node.as_ref() == other
    }
}

impl<T> Ranged<T> for Parsed<T> {
    fn node(&self) -> &T {
        self.node.as_ref()
    }

    fn range(&self) -> Option<(usize, usize)> {
        self.range
    }
}

impl<T> Parsed<T> {
    pub(crate) fn new(node: T, range: Option<(usize, usize)>) -> Self {
        Self {
            node: Box::new(node),
            range,
        }
    }

    pub(crate) fn take(self) -> T {
        *self.node
    }

    pub(crate) fn take_as<U>(self, f: impl FnOnce(T) -> U) -> Parsed<U> {
        Parsed::new(f(*self.node), self.range)
    }
}

pub(super) fn parsed_span<'a, 'b>(
    s: &'a str,
) -> impl FnMut(Span<'b>) -> IResult<Span<'b>, Parsed<&'b str>> + 'a
where
    'b: 'a,
{
    map(tag(s), |t: Span<'b>| {
        let start = t.location_offset();
        let range = Some((start, start + s.len()));
        Parsed::new(*t.fragment(), range)
    })
}

#[cfg(test)]
pub(super) mod strip_ranges {
    use apollo_compiler::collections::IndexMap;

    use super::super::known_var::KnownVariable;
    use super::super::lit_expr::LitExpr;
    use super::super::parser::*;
    use super::Parsed;
    use super::Ranged;

    /// Including location information in the AST introduces unnecessary
    /// varation in many tests. StripLoc is a test-only trait allowing
    /// participating AST nodes to remove their own and their descendants'
    /// location information, thereby normalizing the AST for assert_eq!
    /// comparisons.
    pub trait StripRanges {
        fn strip_ranges(&self) -> Self;
    }

    impl StripRanges for Parsed<String> {
        fn strip_ranges(&self) -> Self {
            Parsed::new(self.node().clone(), None)
        }
    }

    impl StripRanges for JSONSelection {
        fn strip_ranges(&self) -> Self {
            match self {
                JSONSelection::Named(subselect) => JSONSelection::Named(subselect.strip_ranges()),
                JSONSelection::Path(path) => JSONSelection::Path(path.strip_ranges()),
            }
        }
    }

    impl StripRanges for Parsed<JSONSelection> {
        fn strip_ranges(&self) -> Self {
            Parsed::new(
                match self.as_ref() {
                    JSONSelection::Named(subselect) => {
                        JSONSelection::Named(subselect.strip_ranges())
                    }
                    JSONSelection::Path(path) => JSONSelection::Path(path.strip_ranges()),
                },
                None,
            )
        }
    }

    impl StripRanges for NamedSelection {
        fn strip_ranges(&self) -> Self {
            match self {
                Self::Field(alias, key, sub) => Self::Field(
                    alias.as_ref().map(|a| a.strip_ranges()),
                    key.strip_ranges(),
                    sub.as_ref().map(|s| s.strip_ranges()),
                ),
                Self::Path(alias, path) => Self::Path(alias.strip_ranges(), path.strip_ranges()),
                Self::Group(alias, sub) => Self::Group(alias.strip_ranges(), sub.strip_ranges()),
            }
        }
    }

    impl StripRanges for PathSelection {
        fn strip_ranges(&self) -> Self {
            Self {
                path: self.path.strip_ranges(),
            }
        }
    }

    impl StripRanges for Parsed<PathList> {
        fn strip_ranges(&self) -> Self {
            Parsed::new(
                match self.as_ref() {
                    PathList::Var(var, rest) => {
                        PathList::Var(var.strip_ranges(), rest.strip_ranges())
                    }
                    PathList::Key(key, rest) => {
                        PathList::Key(key.strip_ranges(), rest.strip_ranges())
                    }
                    PathList::Method(method, opt_args, rest) => PathList::Method(
                        method.strip_ranges(),
                        opt_args.as_ref().map(|args| args.strip_ranges()),
                        rest.strip_ranges(),
                    ),
                    PathList::Selection(sub) => PathList::Selection(sub.strip_ranges()),
                    PathList::Empty => PathList::Empty,
                },
                None,
            )
        }
    }

    impl StripRanges for SubSelection {
        fn strip_ranges(&self) -> Self {
            SubSelection {
                selections: self.selections.iter().map(|s| s.strip_ranges()).collect(),
                star: self.star.as_ref().map(|s| s.strip_ranges()),
                ..Default::default()
            }
        }
    }

    impl StripRanges for Parsed<StarSelection> {
        fn strip_ranges(&self) -> Self {
            Parsed::new(
                StarSelection(
                    self.0.as_ref().map(|a| a.strip_ranges()),
                    self.1.as_ref().map(|s| s.strip_ranges()),
                ),
                None,
            )
        }
    }

    impl StripRanges for Alias {
        fn strip_ranges(&self) -> Self {
            Alias {
                name: self.name.strip_ranges(),
                range: None,
            }
        }
    }

    impl StripRanges for Parsed<Key> {
        fn strip_ranges(&self) -> Self {
            Parsed::new(self.node().clone(), None)
        }
    }

    impl StripRanges for Parsed<MethodArgs> {
        fn strip_ranges(&self) -> Self {
            Parsed::new(
                MethodArgs(self.0.iter().map(|arg| arg.strip_ranges()).collect()),
                None,
            )
        }
    }

    impl StripRanges for Parsed<LitExpr> {
        fn strip_ranges(&self) -> Self {
            Parsed::new(
                match self.as_ref() {
                    LitExpr::String(s) => LitExpr::String(s.clone()),
                    LitExpr::Number(n) => LitExpr::Number(n.clone()),
                    LitExpr::Bool(b) => LitExpr::Bool(*b),
                    LitExpr::Null => LitExpr::Null,
                    LitExpr::Object(map) => {
                        let mut new_map = IndexMap::default();
                        for (key, value) in map {
                            new_map.insert(key.strip_ranges(), value.strip_ranges());
                        }
                        LitExpr::Object(new_map)
                    }
                    LitExpr::Array(vec) => {
                        let mut new_vec = vec![];
                        for value in vec {
                            new_vec.push(value.strip_ranges());
                        }
                        LitExpr::Array(new_vec)
                    }
                    LitExpr::Path(path) => LitExpr::Path(path.strip_ranges()),
                },
                None,
            )
        }
    }

    impl StripRanges for Parsed<KnownVariable> {
        fn strip_ranges(&self) -> Self {
            Parsed::new(self.node().clone(), None)
        }
    }
}

#[cfg(test)]
mod tests {
    use insta::assert_snapshot;

    use super::*;
    use crate::sources::connect::JSONSelection;

    #[test]
    fn test_merge_ranges() {
        assert_eq!(merge_ranges(None, None), None);
        assert_eq!(merge_ranges(Some((0, 1)), None), Some((0, 1)));
        assert_eq!(merge_ranges(None, Some((0, 1))), Some((0, 1)));
        assert_eq!(merge_ranges(Some((0, 1)), Some((1, 2))), Some((0, 2)));
    }

    #[test]
    fn test_parse_with_range_snapshots() {
        let (remainder, parsed) = JSONSelection::parse(
            r#"
        path: some.nested.path { isbn author { name }}
        alias: "not an identifier" {
            # Inject "Frog" as the __typename
            __typename: @->echo( "Frog" , )
            wrapped: $->echo({ wrapped : @ , })
            group: { a b c }
            arg: $args . arg
            field
            rest: * { data }
        }
        "#,
        )
        .unwrap();
        assert_eq!(remainder, "");
        assert_snapshot!(format!("{:#?}", parsed));
    }
}
