use crate::sources::connect::expand::visitors::FieldVisitor;
use crate::sources::connect::expand::visitors::GroupVisitor;
use crate::sources::connect::json_selection::LitExpr;
use crate::sources::connect::json_selection::MethodArgs;
use crate::sources::connect::json_selection::NamedSelection;
use crate::sources::connect::json_selection::PathList;
use crate::sources::connect::JSONSelection;
use crate::sources::connect::SubSelection;

/// A JSON Selection visitor
pub(super) trait Visitor {
    type Error;

    /// Visit a JSON Selection part. The visitor is guaranteed that all children of this part will
    /// be visited before [`Visitor::end_visit`] is called. A mutable reference is required since
    /// the visitor will typically require modifying internal state.
    fn visit(&mut self, part: &SelectionPart) -> Result<(), Self::Error>;

    /// End visiting a JSON Selection part. This is called after all children of a part have been
    /// visited. The default implementation does nothing.
    fn end_visit(&mut self, _part: &SelectionPart) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// A part of a JSON Selection to be visited
#[derive(Clone, Debug)]
pub(super) enum SelectionPart<'schema> {
    JSONSelection(&'schema JSONSelection),
    LitExpr(&'schema LitExpr),
    MethodArgs(&'schema MethodArgs),
    NamedSelection(&'schema NamedSelection),
    PathList(&'schema PathList),
    SubSelection(&'schema SubSelection),
}

/// Visit a JSON Selection, invoking a visitor on each part
pub(super) fn visit<V: Visitor>(selection: &JSONSelection, mut visitor: V) -> Result<(), V::Error> {
    SelectionWalker {
        visitor: &mut visitor,
        stack: vec![],
    }
    .walk(SelectionGroup::Root {
        children: vec![SelectionPart::JSONSelection(selection)],
    })
}

/// Walks a JSON Selection and invokes a visitor
struct SelectionWalker<'schema, V: Visitor> {
    pub(super) visitor: &'schema mut V,
    pub(super) stack: Vec<SelectionPart<'schema>>,
}

enum SelectionGroup<'schema> {
    Root {
        children: Vec<SelectionPart<'schema>>,
    },
    Child {
        parent: SelectionPart<'schema>,
        children: Vec<SelectionPart<'schema>>,
    },
}

impl<'schema> SelectionGroup<'schema> {
    fn new(parent: SelectionPart<'schema>, children: Vec<SelectionPart<'schema>>) -> Self {
        SelectionGroup::Child { parent, children }
    }

    fn empty(parent: SelectionPart<'schema>) -> Self {
        SelectionGroup::Child {
            parent,
            children: vec![],
        }
    }

    fn children(&self) -> Vec<SelectionPart<'schema>> {
        match self {
            SelectionGroup::Root { children } => children.clone(),
            SelectionGroup::Child { children, .. } => children.clone(),
        }
    }
}

impl<'schema, V: Visitor> GroupVisitor<SelectionGroup<'schema>, SelectionPart<'schema>>
    for SelectionWalker<'schema, V>
{
    fn try_get_group_for_field(
        &self,
        field: &SelectionPart<'schema>,
    ) -> Result<Option<SelectionGroup<'schema>>, Self::Error> {
        // Leaf nodes should return [`SelectionGroup::Empty`] rather than `None` to ensure that
        // `exit_group` is called on the empty group, which in turn exits the visitor.
        let field = field.clone();
        let result = Ok(match field {
            SelectionPart::JSONSelection(json_selection) => match json_selection {
                JSONSelection::Named(sub_selection) => Some(SelectionGroup::new(
                    field,
                    vec![SelectionPart::SubSelection(sub_selection)],
                )),
                JSONSelection::Path(path_selection) => Some(SelectionGroup::new(
                    field,
                    vec![SelectionPart::PathList(&path_selection.path)],
                )),
            },
            SelectionPart::LitExpr(lit_expr) => match lit_expr {
                LitExpr::Path(path_selection) => Some(SelectionGroup::new(
                    field,
                    vec![SelectionPart::PathList(&path_selection.path)],
                )),
                LitExpr::Object(obj) => Some(SelectionGroup::new(
                    field,
                    obj.values()
                        .collect::<Vec<_>>()
                        .iter()
                        .map(|value| SelectionPart::LitExpr(value))
                        .collect(),
                )),
                LitExpr::Array(array) => Some(SelectionGroup::new(
                    field,
                    array
                        .iter()
                        .map(|value| SelectionPart::LitExpr(value))
                        .collect(),
                )),
                LitExpr::String(_) | LitExpr::Number(_) | LitExpr::Bool(_) | LitExpr::Null => {
                    Some(SelectionGroup::empty(field))
                }
            },
            SelectionPart::NamedSelection(selection) => match selection {
                NamedSelection::Field(_, _, Some(sub_selection)) => Some(SelectionGroup::new(
                    field,
                    vec![SelectionPart::SubSelection(sub_selection)],
                )),
                NamedSelection::Path(_, path_selection) => Some(SelectionGroup::new(
                    field,
                    vec![SelectionPart::PathList(&path_selection.path)],
                )),
                NamedSelection::Group(_, sub_selection) => Some(SelectionGroup::new(
                    field,
                    vec![SelectionPart::SubSelection(sub_selection)],
                )),
                NamedSelection::Field(_, _, None) => Some(SelectionGroup::empty(field)),
            },
            SelectionPart::MethodArgs(args) => Some(SelectionGroup::new(
                field,
                args.args
                    .iter()
                    .map(|value| SelectionPart::LitExpr(value))
                    .collect(),
            )),
            SelectionPart::PathList(path_list) => match path_list {
                PathList::Var(_, path_list) => Some(SelectionGroup::new(
                    field,
                    vec![SelectionPart::PathList(path_list)],
                )),
                PathList::Key(_, path_list) => Some(SelectionGroup::new(
                    field,
                    vec![SelectionPart::PathList(path_list)],
                )),
                PathList::Expr(lit, path_list) => Some(SelectionGroup::new(
                    field,
                    vec![
                        SelectionPart::LitExpr(lit),
                        SelectionPart::PathList(path_list),
                    ],
                )),
                PathList::Method(_, args, path_list) => {
                    let mut children = vec![];
                    if let Some(args) = args {
                        children.push(SelectionPart::MethodArgs(args));
                    }
                    children.push(SelectionPart::PathList(path_list));
                    Some(SelectionGroup::new(field, children))
                }
                PathList::Selection(sub_selection) => Some(SelectionGroup::new(
                    field,
                    vec![SelectionPart::SubSelection(sub_selection)],
                )),
                PathList::Empty => Some(SelectionGroup::empty(field)),
            },
            SelectionPart::SubSelection(selection) => Some(SelectionGroup::new(
                field,
                selection
                    .selections_iter()
                    .map(SelectionPart::NamedSelection)
                    .collect(),
            )),
        });
        result
    }

    fn enter_group(
        &mut self,
        group: &SelectionGroup<'schema>,
    ) -> Result<Vec<SelectionPart<'schema>>, Self::Error> {
        match group {
            SelectionGroup::Child { parent, .. } => self.stack.push(parent.clone()),
            SelectionGroup::Root { .. } => {}
        }
        Ok(group.children())
    }

    fn exit_group(&mut self) -> Result<(), Self::Error> {
        if let Some(field) = self.stack.pop() {
            self.visitor.end_visit(&field)?;
        }
        Ok(())
    }
}

impl<'schema, V: Visitor> FieldVisitor<SelectionPart<'schema>> for SelectionWalker<'schema, V> {
    type Error = V::Error;

    fn visit(&mut self, field: SelectionPart<'schema>) -> Result<(), Self::Error> {
        self.visitor.visit(&field)
    }
}

#[cfg(test)]
mod tests {
    use super::visit;
    use super::SelectionPart;
    use super::Visitor;
    use crate::sources::connect::json_selection::PathList;
    use crate::sources::connect::JSONSelection;

    #[test]
    fn test_walk() {
        let mut entered = vec![];
        let mut exited = vec![];

        struct TestVisitor<'a> {
            entered: &'a mut Vec<(String, usize)>,
            exited: &'a mut Vec<(String, usize)>,
        }
        impl<'a> Visitor for TestVisitor<'a> {
            type Error = ();

            fn visit(&mut self, part: &SelectionPart) -> Result<(), Self::Error> {
                if let SelectionPart::PathList(PathList::Method(name, args, _)) = part {
                    self.entered.push((
                        name.to_string(),
                        args.as_ref().map(|args| args.args.len()).unwrap_or(0),
                    ));
                }
                Ok(())
            }

            fn end_visit(&mut self, part: &SelectionPart) -> Result<(), Self::Error> {
                if let SelectionPart::PathList(PathList::Method(name, args, _)) = part {
                    self.exited.push((
                        name.to_string(),
                        args.as_ref().map(|args| args.args.len()).unwrap_or(0),
                    ));
                }
                Ok(())
            }
        }

        let (_, selection) =
            JSONSelection::parse(r#"id name alias: foo->match(["one", one->first->size])"#)
                .unwrap();

        let visitor = TestVisitor {
            entered: &mut entered,
            exited: &mut exited,
        };
        assert_eq!(Ok(()), visit(&selection, visitor));
        assert_eq!(
            &[
                (String::from("match"), 1usize),
                (String::from("first"), 0usize),
                (String::from("size"), 0usize)
            ],
            &entered.as_slice()
        );
        assert_eq!(
            &[
                (String::from("size"), 0usize),
                (String::from("first"), 0usize),
                (String::from("match"), 1usize)
            ],
            &exited.as_slice()
        );
    }
}
