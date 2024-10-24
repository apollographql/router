use nom::bytes::complete::tag;
use nom::combinator::map;
use nom_locate::LocatedSpan;

use super::ParseResult;

// Currently, all our error messages are &'static str, which allows the Span
// type to remain Copy, which is convenient to avoid having to clone Spans
// frequently in the parser code.
//
// If we wanted to introduce any error messages computed using format!, we'd
// have to switch to Option<String> here (or some other type containing owned
// String data), which would make Span no longer Copy, requiring more cloning.
// Not the end of the world, but something to keep in mind for the future.
//
// The cloning would still be relatively cheap because we use None throughout
// parsing and then only set Some(message) when we need to report an error, so
// we would not be cloning long String messages very often (and the rest of the
// Span fields are cheap to clone).
pub(crate) type Span<'a> = LocatedSpan<&'a str, Option<&'static str>>;

pub(super) fn new_span(input: &str) -> Span {
    Span::new_extra(input, None)
}

// Some parsed AST structures, like PathSelection and NamedSelection, can
// produce a range directly from their children, so they do not need to be
// wrapped as WithRange<PathSelection> or WithRange<NamedSelection>.
// Additionally, AST nodes that are structs can store their own range as a
// field, so they can implement Ranged<T> without the WithRange<T> wrapper.
pub(crate) trait Ranged<T> {
    fn range(&self) -> OffsetRange;
}

// The ranges produced by the JSONSelection parser are pairs of character
// offsets into the original string. The first element of the pair is the offset
// of the first character, and the second element is the offset of the character
// just past the end of the range. Offsets start at 0 for the first character in
// the file, following nom_locate's span.location_offset() convention.
pub(crate) type OffsetRange = Option<std::ops::Range<usize>>;

// The most common implementation of the Ranged<T> trait is the WithRange<T>
// struct, used to wrap any AST node that (a) needs its own location information
// (because that information is not derivable from its children) and (b) cannot
// easily store that information by adding another struct field (most often
// because T is an enum or primitive/String type, not a struct).
#[derive(Debug, PartialEq, Eq, Clone, Hash)]
pub struct WithRange<T> {
    node: Box<T>,
    range: OffsetRange,
}

// We can recover some of the ergonomics of working with the inner type T by
// implementing Deref and DerefMut for WithRange<T>.
impl<T> std::ops::Deref for WithRange<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.node.as_ref()
    }
}
impl<T> std::ops::DerefMut for WithRange<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.node.as_mut()
    }
}

impl<T> AsRef<T> for WithRange<T> {
    fn as_ref(&self) -> &T {
        self.node.as_ref()
    }
}

impl<T> AsMut<T> for WithRange<T> {
    fn as_mut(&mut self) -> &mut T {
        self.node.as_mut()
    }
}

impl<T> PartialEq<T> for WithRange<T>
where
    T: PartialEq,
{
    fn eq(&self, other: &T) -> bool {
        self.node.as_ref() == other
    }
}

impl<T> Ranged<T> for WithRange<T> {
    fn range(&self) -> OffsetRange {
        self.range.clone()
    }
}

impl<T> WithRange<T> {
    pub(crate) fn new(node: T, range: OffsetRange) -> Self {
        Self {
            node: Box::new(node),
            range,
        }
    }

    #[allow(unused)]
    pub(crate) fn take(self) -> T {
        *self.node
    }

    pub(crate) fn take_as<U>(self, f: impl FnOnce(T) -> U) -> WithRange<U> {
        WithRange::new(f(*self.node), self.range)
    }
}

pub(super) fn merge_ranges(left: OffsetRange, right: OffsetRange) -> OffsetRange {
    match (left, right) {
        // Tolerate out-of-order and overlapping ranges.
        (Some(left_range), Some(right_range)) => {
            Some(left_range.start.min(right_range.start)..left_range.end.max(right_range.end))
        }
        (Some(left_range), None) => Some(left_range),
        (None, Some(right_range)) => Some(right_range),
        (None, None) => None,
    }
}

// Parser combinator that matches a &str and returns a WithRange<&str> with the
// matched string and the range of the match.
pub(super) fn ranged_span<'a, 'b>(
    s: &'a str,
) -> impl FnMut(Span<'b>) -> ParseResult<WithRange<&'b str>> + 'a
where
    'b: 'a,
{
    map(tag(s), |t: Span<'b>| {
        let start = t.location_offset();
        let range = Some(start..start + s.len());
        WithRange::new(*t.fragment(), range)
    })
}

#[cfg(test)]
pub(crate) mod strip_ranges {
    use apollo_compiler::collections::IndexMap;

    use super::super::known_var::KnownVariable;
    use super::super::lit_expr::LitExpr;
    use super::super::parser::*;
    use super::WithRange;

    /// Including location information in the AST introduces unnecessary
    /// varation in many tests. StripLoc is a test-only trait allowing
    /// participating AST nodes to remove their own and their descendants'
    /// location information, thereby normalizing the AST for assert_eq!
    /// comparisons.
    pub(crate) trait StripRanges {
        fn strip_ranges(&self) -> Self;
    }

    impl StripRanges for WithRange<String> {
        fn strip_ranges(&self) -> Self {
            WithRange::new(self.as_ref().clone(), None)
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

    impl StripRanges for NamedSelection {
        fn strip_ranges(&self) -> Self {
            match self {
                Self::Field(alias, key, sub) => Self::Field(
                    alias.as_ref().map(|a| a.strip_ranges()),
                    key.strip_ranges(),
                    sub.as_ref().map(|s| s.strip_ranges()),
                ),
                Self::Path(alias, path) => {
                    let stripped_alias = alias.as_ref().map(|a| a.strip_ranges());
                    Self::Path(stripped_alias, path.strip_ranges())
                }
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

    impl StripRanges for WithRange<PathList> {
        fn strip_ranges(&self) -> Self {
            WithRange::new(
                match self.as_ref() {
                    PathList::Var(var, rest) => {
                        PathList::Var(var.strip_ranges(), rest.strip_ranges())
                    }
                    PathList::Key(key, rest) => {
                        PathList::Key(key.strip_ranges(), rest.strip_ranges())
                    }
                    PathList::Expr(expr, rest) => {
                        PathList::Expr(expr.strip_ranges(), rest.strip_ranges())
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
                ..Default::default()
            }
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

    impl StripRanges for WithRange<Key> {
        fn strip_ranges(&self) -> Self {
            WithRange::new(self.as_ref().clone(), None)
        }
    }

    impl StripRanges for MethodArgs {
        fn strip_ranges(&self) -> Self {
            MethodArgs {
                args: self.args.iter().map(|arg| arg.strip_ranges()).collect(),
                range: None,
            }
        }
    }

    impl StripRanges for WithRange<LitExpr> {
        fn strip_ranges(&self) -> Self {
            WithRange::new(
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

    impl StripRanges for WithRange<KnownVariable> {
        fn strip_ranges(&self) -> Self {
            WithRange::new(self.as_ref().clone(), None)
        }
    }
}

#[cfg(test)]
mod tests {
    use insta::assert_debug_snapshot;
    use insta::assert_snapshot;

    use super::*;
    use crate::sources::connect::JSONSelection;

    #[test]
    fn test_merge_ranges() {
        // Simple cases:
        assert_eq!(merge_ranges(None, None), None);
        assert_eq!(merge_ranges(Some(0..1), None), Some(0..1));
        assert_eq!(merge_ranges(None, Some(0..1)), Some(0..1));
        assert_eq!(merge_ranges(Some(0..1), Some(1..2)), Some(0..2));

        // Out-of-order and overlapping ranges:
        assert_eq!(merge_ranges(Some(1..2), Some(0..1)), Some(0..2));
        assert_eq!(merge_ranges(Some(0..1), Some(1..2)), Some(0..2));
        assert_eq!(merge_ranges(Some(0..2), Some(1..3)), Some(0..3));
        assert_eq!(merge_ranges(Some(1..3), Some(0..2)), Some(0..3));
    }

    #[test]
    fn test_arrow_path_ranges() {
        let (remainder, parsed) =
            JSONSelection::parse("  __typename: @ -> echo ( \"Frog\" , )  ").unwrap();
        assert_eq!(remainder, "");
        assert_debug_snapshot!(parsed);
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
        }
        "#,
        )
        .unwrap();
        assert_eq!(remainder, "");
        assert_snapshot!(format!("{:#?}", parsed));
    }
}
