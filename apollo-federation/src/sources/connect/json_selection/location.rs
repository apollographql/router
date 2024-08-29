use apollo_compiler::collections::IndexMap;
use nom::bytes::complete::tag;
use nom::combinator::map;
use nom::IResult;

use super::lit_expr::LitExpr;
use super::Alias;
use super::JSONSelection;
use super::Key;
use super::KnownVariable;
use super::MethodArgs;
use super::NamedSelection;
use super::PathList;
use super::PathSelection;
use super::StarSelection;
use super::SubSelection;

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

pub(super) trait Located {
    fn loc(&self) -> Option<(usize, usize)>;
    fn children(&self) -> Vec<&dyn Located>;
}

pub(super) trait StripLoc {
    fn strip_loc(&self) -> Self;
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

pub(super) fn parsed_tag<'a, 'b>(
    s: &'a str,
) -> impl FnMut(&'b str) -> IResult<&'b str, Parsed<&'b str>> + 'a
where
    'b: 'a,
{
    map(tag(s), |t: &str| Parsed::new(t, None))
}

impl Located for Parsed<String> {
    fn loc(&self) -> Option<(usize, usize)> {
        self.loc()
    }

    fn children(&self) -> Vec<&dyn Located> {
        vec![]
    }
}

impl StripLoc for Parsed<String> {
    fn strip_loc(&self) -> Self {
        Parsed::new(self.node().clone(), None)
    }
}

impl Located for Parsed<JSONSelection> {
    fn loc(&self) -> Option<(usize, usize)> {
        self.loc()
    }

    fn children(&self) -> Vec<&dyn Located> {
        match self.as_ref() {
            JSONSelection::Named(subselect) => vec![subselect],
            JSONSelection::Path(path) => vec![path],
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

impl Located for Parsed<NamedSelection> {
    fn loc(&self) -> Option<(usize, usize)> {
        self.loc()
    }

    fn children(&self) -> Vec<&dyn Located> {
        match self.as_ref() {
            NamedSelection::Field(alias, key, sub) => {
                let mut children: Vec<&dyn Located> = vec![key];
                if let Some(alias) = alias {
                    children.push(alias);
                }
                if let Some(sub) = sub {
                    children.push(sub);
                }
                children
            }
            NamedSelection::Path(alias, path) => vec![alias, path],
            NamedSelection::Group(alias, sub) => vec![alias, sub],
        }
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

impl Located for Parsed<PathSelection> {
    fn loc(&self) -> Option<(usize, usize)> {
        self.loc()
    }

    fn children(&self) -> Vec<&dyn Located> {
        vec![&self.path]
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

impl Located for Parsed<PathList> {
    fn loc(&self) -> Option<(usize, usize)> {
        self.loc()
    }

    fn children(&self) -> Vec<&dyn Located> {
        match self.as_ref() {
            PathList::Var(var, rest) => vec![var, rest],
            PathList::Key(key, rest) => vec![key, rest],
            PathList::Method(method, opt_args, rest) => {
                let mut children: Vec<&dyn Located> = vec![method];
                if let Some(args) = opt_args {
                    children.push(args);
                }
                children.push(rest);
                children
            }
            PathList::Selection(sub) => vec![sub],
            PathList::Empty => vec![],
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

impl Located for Parsed<SubSelection> {
    fn loc(&self) -> Option<(usize, usize)> {
        self.loc()
    }

    fn children(&self) -> Vec<&dyn Located> {
        let mut children = self
            .selections
            .iter()
            .map(|s| s as &dyn Located)
            .collect::<Vec<_>>();
        if let Some(star) = &self.star {
            children.push(star);
        }
        children
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

impl Located for Parsed<StarSelection> {
    fn loc(&self) -> Option<(usize, usize)> {
        self.loc()
    }

    fn children(&self) -> Vec<&dyn Located> {
        let mut children: Vec<&dyn Located> = vec![];
        if let Some(alias) = &self.0 {
            children.push(alias);
        }
        if let Some(sub) = &self.1 {
            children.push(sub);
        }
        children
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

impl Located for Parsed<Alias> {
    fn loc(&self) -> Option<(usize, usize)> {
        self.loc()
    }

    fn children(&self) -> Vec<&dyn Located> {
        vec![&self.name]
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

impl Located for Parsed<Key> {
    fn loc(&self) -> Option<(usize, usize)> {
        self.loc()
    }

    fn children(&self) -> Vec<&dyn Located> {
        vec![]
    }
}

impl StripLoc for Parsed<Key> {
    fn strip_loc(&self) -> Self {
        Parsed::new(self.node().clone(), None)
    }
}

impl Located for Parsed<MethodArgs> {
    fn loc(&self) -> Option<(usize, usize)> {
        self.loc()
    }

    fn children(&self) -> Vec<&dyn Located> {
        self.0.iter().map(|arg| arg as &dyn Located).collect()
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

impl Located for Parsed<LitExpr> {
    fn loc(&self) -> Option<(usize, usize)> {
        self.loc
    }

    fn children(&self) -> Vec<&dyn Located> {
        match self.as_ref() {
            LitExpr::String(_) | LitExpr::Number(_) | LitExpr::Bool(_) | LitExpr::Null => vec![],
            LitExpr::Object(map) => {
                let mut children: Vec<&dyn Located> = vec![];
                for (key, value) in map {
                    children.push(key);
                    children.push(value);
                }
                children
            }
            LitExpr::Array(vec) => vec.iter().map(|v| v as &dyn Located).collect(),
            LitExpr::Path(path) => vec![&path.path],
        }
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

impl Located for Parsed<KnownVariable> {
    fn loc(&self) -> Option<(usize, usize)> {
        self.loc()
    }

    fn children(&self) -> Vec<&dyn Located> {
        vec![]
    }
}

impl StripLoc for Parsed<KnownVariable> {
    fn strip_loc(&self) -> Self {
        Parsed::new(self.node().clone(), None)
    }
}
