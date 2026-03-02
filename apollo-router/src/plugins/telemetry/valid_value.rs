use std::sync::LazyLock;

use regex::Regex;

// Regex for allowed values for client and library names and versions
static VALID_VALUE_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[ a-zA-Z0-9.@/_\-]{1,60}$").unwrap());

pub(super) trait ValidValue {
    fn is_valid_value(&self) -> bool;
}

impl ValidValue for str {
    fn is_valid_value(&self) -> bool {
        VALID_VALUE_REGEX.is_match(self)
    }
}
