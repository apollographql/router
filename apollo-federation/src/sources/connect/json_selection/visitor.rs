//! JSONSelection Visitor
//!
//! In many cases it can be useful to visit all of the output keys in a JSONSelection.
//! This module defines a trait which allows for defining custom handling logic over
//! all output keys and their (optional) subkeys.

use std::collections::VecDeque;

use itertools::Itertools;

use crate::error::FederationError;

use super::JSONSelection;

pub trait JSONSelectionVisitor {
    fn visit(&mut self, name: &str) -> Result<(), FederationError>;

    fn enter_group(&mut self, group: &str) -> Result<(), FederationError>;
    fn exit_group(&mut self) -> Result<(), FederationError>;

    fn finish(self) -> Result<(), FederationError>;
}

impl JSONSelection {
    pub fn visit(&self, mut visitor: impl JSONSelectionVisitor) -> Result<(), FederationError> {
        let primed = match &self {
            JSONSelection::Named(named) => named.selections.iter(),
            JSONSelection::Path(path) => path
                .next_subselection()
                .map(|sub| sub.selections.iter())
                .unwrap_or([].iter()),
        };
        let mut to_visit = VecDeque::from_iter(
            primed
                .sorted_by(|a, b| Ord::cmp(a.name(), b.name()))
                .map(|n| (0, n)),
        );

        // Start visiting each of the selections
        let mut current_depth = 0;
        while let Some((depth, next)) = to_visit.pop_front() {
            if depth < current_depth {
                visitor.exit_group()?;
                current_depth = depth;
            }

            visitor.visit(next.name())?;

            // If we have a named selection that has a subselection, then we want to
            // make sure that we visit the children before all other siblings.
            //
            // Note: We sort by the reverse order here since we always push to the front.
            if let Some(sub) = next.next_subselection() {
                current_depth += 1;
                visitor.enter_group(next.name())?;
                sub.selections
                    .iter()
                    .sorted_by(|a, b| Ord::cmp(b.name(), a.name()))
                    .for_each(|s| to_visit.push_front((current_depth, s)));
            }
        }

        // Make sure that we exit until we are no longer nested
        for _ in 0..current_depth {
            visitor.exit_group()?;
        }

        // Finish out the visitor
        visitor.finish()
    }
}

#[cfg(test)]
mod tests {
    use insta::assert_snapshot;

    use crate::sources::connect::JSONSelection;

    use super::JSONSelectionVisitor;

    /// Visitor for tests.
    ///
    /// Each node visited is added, along with its depth. This is later printed
    /// such that groups are indented based on depth.
    struct TestVisitor<'a> {
        depth_stack: Vec<usize>,
        visited: &'a mut Vec<(usize, String)>,
    }

    impl<'a> TestVisitor<'a> {
        fn new(visited: &'a mut Vec<(usize, String)>) -> Self {
            Self {
                depth_stack: Vec::new(),
                visited,
            }
        }

        fn last_depth(&self) -> usize {
            *self.depth_stack.last().unwrap_or(&0)
        }
    }

    fn print_visited(visited: Vec<(usize, String)>) -> String {
        let mut result = String::new();
        for (depth, visited) in visited {
            result.push_str(&format!("{}{visited}\n", "|  ".repeat(depth)));
        }

        result
    }

    impl JSONSelectionVisitor for TestVisitor<'_> {
        fn visit(&mut self, name: &str) -> Result<(), crate::error::FederationError> {
            self.visited.push((self.last_depth(), name.to_string()));

            Ok(())
        }

        fn enter_group(&mut self, _: &str) -> Result<(), crate::error::FederationError> {
            self.depth_stack.push(self.last_depth() + 1);
            Ok(())
        }

        fn exit_group(&mut self) -> Result<(), crate::error::FederationError> {
            self.depth_stack.pop().unwrap();
            Ok(())
        }

        fn finish(self) -> Result<(), crate::error::FederationError> {
            Ok(())
        }
    }

    #[test]
    fn it_iterates_over_empty_path() {
        let mut visited = Vec::new();
        let visitor = TestVisitor::new(&mut visited);
        let (unmatched, selection) = JSONSelection::parse("").unwrap();
        assert!(unmatched.is_empty());

        selection.visit(visitor).unwrap();
        assert_snapshot!(print_visited(visited), @"");
    }

    #[test]
    fn it_iterates_over_simple_selection() {
        let mut visited = Vec::new();
        let visitor = TestVisitor::new(&mut visited);
        let (unmatched, selection) = JSONSelection::parse("a b c d").unwrap();
        assert!(unmatched.is_empty());

        selection.visit(visitor).unwrap();
        assert_snapshot!(print_visited(visited), @r###"
        a
        b
        c
        d
        "###);
    }

    #[test]
    fn it_iterates_over_aliased_selection() {
        let mut visited = Vec::new();
        let visitor = TestVisitor::new(&mut visited);
        let (unmatched, selection) =
            JSONSelection::parse("a: one b: two c: three d: four").unwrap();
        assert!(unmatched.is_empty());

        selection.visit(visitor).unwrap();
        assert_snapshot!(print_visited(visited), @r###"
        a
        b
        c
        d
        "###);
    }

    #[test]
    fn it_iterates_over_nested_selection() {
        let mut visited = Vec::new();
        let visitor = TestVisitor::new(&mut visited);
        let (unmatched, selection) = JSONSelection::parse("a { b { c { d { e } } } }").unwrap();
        assert!(unmatched.is_empty());

        selection.visit(visitor).unwrap();
        assert_snapshot!(print_visited(visited), @r###"
        a
        |  b
        |  |  c
        |  |  |  d
        |  |  |  |  e
        "###);
    }

    #[test]
    fn it_iterates_over_complex_selection() {
        let mut visited = Vec::new();
        let visitor = TestVisitor::new(&mut visited);
        let (unmatched, selection) = JSONSelection::parse(
            "id
            name
            username
            email
            address {
              street
              suite
              city
              zipcode
              geo {
                lat
                lng
              }
            }
            phone
            website
            company {
              name
              catchPhrase
              bs
            }",
        )
        .unwrap();
        assert!(unmatched.is_empty());

        selection.visit(visitor).unwrap();
        assert_snapshot!(print_visited(visited), @r###"
        address
        |  city
        |  geo
        |  |  lat
        |  |  lng
        |  street
        |  suite
        |  zipcode
        company
        |  bs
        |  catchPhrase
        |  name
        email
        id
        name
        phone
        username
        website
        "###);
        // let iter = selection.iter();
        // assert_debug_snapshot!(iter.collect_vec());
    }
}
