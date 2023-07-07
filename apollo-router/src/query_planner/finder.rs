#[derive(Debug)]
pub(crate) struct Finder {
  pub(crate) query_name: String,
  pub(crate) param_name: String,
  pub(crate) param_type: String,
}

impl Finder {
  pub(crate) fn new(
    query_name: &str,
    param_name: &str,
    param_type: &str,
  ) -> Self {
    Self {
      query_name: query_name.to_string(),
      param_name: param_name.to_string(),
      param_type: param_type.to_string(),
    }
  }
}

pub(crate) fn make_finder_index(s1: &str, s2: &str) -> String {
  format!("{}+{}", s1, s2)
}

#[test]
fn test_make_finder_index() {
    let s1 = "Subgraph1";
    let s2 = "User";
    let expected = "Subgraph1+User".to_string();
    let result = make_finder_index(s1, s2);
    assert_eq!(result, expected);
}