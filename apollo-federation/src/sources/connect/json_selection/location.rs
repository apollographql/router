use nom::bytes::complete::tag;
use nom::combinator::map;
use nom::IResult;
use nom_locate::LocatedSpan;

pub type Span<'a> = LocatedSpan<&'a str>;

#[derive(Debug, PartialEq, Eq, Clone, Hash)]
pub struct Parsed<T> {
    node: Box<T>,
    loc: Option<(usize, usize)>,
}

pub(super) fn merge_locs(
    left: Option<(usize, usize)>,
    right: Option<(usize, usize)>,
) -> Option<(usize, usize)> {
    match (left, right) {
        (Some((left_start, _left_end)), Some((_right_start, right_end))) => {
            Some((left_start, right_end))
        }
        (Some(left_loc), None) => Some(left_loc),
        (None, Some(right_loc)) => Some(right_loc),
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

impl<T> Parsed<T> {
    pub(super) fn new(node: T, loc: Option<(usize, usize)>) -> Self {
        Self {
            node: Box::new(node),
            loc,
        }
    }

    pub(crate) fn take(self) -> T {
        *self.node
    }

    pub(crate) fn take_as<U>(self, f: impl FnOnce(T) -> U) -> Parsed<U> {
        Parsed::new(f(*self.node), self.loc)
    }

    pub(crate) fn node(&self) -> &T {
        self.node.as_ref()
    }

    pub(crate) fn loc(&self) -> Option<(usize, usize)> {
        self.loc
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
        let loc = Some((start, start + s.len()));
        Parsed::new(*t.fragment(), loc)
    })
}

#[cfg(test)]
pub(super) mod strip_loc {
    use apollo_compiler::collections::IndexMap;

    use super::super::known_var::KnownVariable;
    use super::super::lit_expr::LitExpr;
    use super::super::parser::*;
    use super::Parsed;

    /// Including location information in the AST introduces unnecessary
    /// varation in many tests. StripLoc is a test-only trait allowing
    /// participating AST nodes to remove their own and their descendants'
    /// location information, thereby normalizing the AST for assert_eq!
    /// comparisons.
    pub trait StripLoc {
        fn strip_loc(&self) -> Self;
    }

    impl StripLoc for Parsed<String> {
        fn strip_loc(&self) -> Self {
            Parsed::new(self.node().clone(), None)
        }
    }

    impl StripLoc for JSONSelection {
        fn strip_loc(&self) -> Self {
            match self {
                JSONSelection::Named(subselect) => JSONSelection::Named(subselect.strip_loc()),
                JSONSelection::Path(path) => JSONSelection::Path(path.strip_loc()),
            }
        }
    }

    impl StripLoc for Parsed<JSONSelection> {
        fn strip_loc(&self) -> Self {
            Parsed::new(
                match self.as_ref() {
                    JSONSelection::Named(subselect) => JSONSelection::Named(subselect.strip_loc()),
                    JSONSelection::Path(path) => JSONSelection::Path(path.strip_loc()),
                },
                None,
            )
        }
    }

    impl StripLoc for Parsed<NamedSelection> {
        fn strip_loc(&self) -> Self {
            Parsed::new(
                match self.as_ref() {
                    NamedSelection::Field(alias, key, sub) => NamedSelection::Field(
                        alias.as_ref().map(|a| a.strip_loc()),
                        key.strip_loc(),
                        sub.as_ref().map(|s| s.strip_loc()),
                    ),
                    NamedSelection::Path(alias, path) => {
                        NamedSelection::Path(alias.strip_loc(), path.strip_loc())
                    }
                    NamedSelection::Group(alias, sub) => {
                        NamedSelection::Group(alias.strip_loc(), sub.strip_loc())
                    }
                },
                None,
            )
        }
    }

    impl StripLoc for Parsed<PathSelection> {
        fn strip_loc(&self) -> Self {
            Parsed::new(self.node().strip_loc(), None)
        }
    }

    impl StripLoc for PathSelection {
        fn strip_loc(&self) -> Self {
            PathSelection {
                path: self.path.strip_loc(),
            }
        }
    }

    impl StripLoc for Parsed<PathList> {
        fn strip_loc(&self) -> Self {
            Parsed::new(
                match self.as_ref() {
                    PathList::Var(var, rest) => PathList::Var(var.strip_loc(), rest.strip_loc()),
                    PathList::Key(key, rest) => PathList::Key(key.strip_loc(), rest.strip_loc()),
                    PathList::Method(method, opt_args, rest) => PathList::Method(
                        method.strip_loc(),
                        opt_args.as_ref().map(|args| args.strip_loc()),
                        rest.strip_loc(),
                    ),
                    PathList::Selection(sub) => PathList::Selection(sub.strip_loc()),
                    PathList::Empty => PathList::Empty,
                },
                None,
            )
        }
    }

    impl StripLoc for Parsed<SubSelection> {
        fn strip_loc(&self) -> Self {
            Parsed::new(
                SubSelection {
                    selections: self.selections.iter().map(|s| s.strip_loc()).collect(),
                    star: self.star.as_ref().map(|s| s.strip_loc()),
                },
                None,
            )
        }
    }

    impl StripLoc for Parsed<StarSelection> {
        fn strip_loc(&self) -> Self {
            Parsed::new(
                StarSelection(
                    self.0.as_ref().map(|a| a.strip_loc()),
                    self.1.as_ref().map(|s| s.strip_loc()),
                ),
                None,
            )
        }
    }

    impl StripLoc for Parsed<Alias> {
        fn strip_loc(&self) -> Self {
            Parsed::new(
                Alias {
                    name: self.name.strip_loc(),
                },
                None,
            )
        }
    }

    impl StripLoc for Parsed<Key> {
        fn strip_loc(&self) -> Self {
            Parsed::new(self.node().clone(), None)
        }
    }

    impl StripLoc for Parsed<MethodArgs> {
        fn strip_loc(&self) -> Self {
            Parsed::new(
                MethodArgs(self.0.iter().map(|arg| arg.strip_loc()).collect()),
                None,
            )
        }
    }

    impl StripLoc for Parsed<LitExpr> {
        fn strip_loc(&self) -> Self {
            Parsed::new(
                match self.as_ref() {
                    LitExpr::String(s) => LitExpr::String(s.clone()),
                    LitExpr::Number(n) => LitExpr::Number(n.clone()),
                    LitExpr::Bool(b) => LitExpr::Bool(*b),
                    LitExpr::Null => LitExpr::Null,
                    LitExpr::Object(map) => {
                        let mut new_map = IndexMap::default();
                        for (key, value) in map {
                            new_map.insert(key.strip_loc(), value.strip_loc());
                        }
                        LitExpr::Object(new_map)
                    }
                    LitExpr::Array(vec) => {
                        let mut new_vec = vec![];
                        for value in vec {
                            new_vec.push(value.strip_loc());
                        }
                        LitExpr::Array(new_vec)
                    }
                    LitExpr::Path(path) => LitExpr::Path(path.strip_loc()),
                },
                None,
            )
        }
    }

    impl StripLoc for Parsed<KnownVariable> {
        fn strip_loc(&self) -> Self {
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
    fn test_merge_locs() {
        assert_eq!(merge_locs(None, None), None);
        assert_eq!(merge_locs(Some((0, 1)), None), Some((0, 1)));
        assert_eq!(merge_locs(None, Some((0, 1))), Some((0, 1)));
        assert_eq!(merge_locs(Some((0, 1)), Some((1, 2))), Some((0, 2)));
    }

    #[test]
    fn test_parse_with_loc_snapshots() {
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
