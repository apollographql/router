pub(crate) struct JoinStringsOptions<'a> {
    pub(crate) separator: &'a str,
    pub(crate) first_separator: Option<&'a str>,
    pub(crate) last_separator: Option<&'a str>,
    /// When displaying a list of something in a human-readable form, after what size (in number of
    /// characters) we start displaying only a subset of the list. Note this only counts characters
    /// in list elements, and ignores separators.
    pub(crate) output_length_limit: Option<usize>,
}

impl Default for JoinStringsOptions<'_> {
    fn default() -> Self {
        Self {
            separator: ", ",
            first_separator: None,
            last_separator: Some(" and "),
            output_length_limit: None,
        }
    }
}

/// Joins an iterator of strings, but with the ability to use a specific different separator for the
/// first and/or last occurrence (if both are given and the list is size two, the first separator is
/// used). Optionally, if the resulting list to print is "too long", it can display a subset of the
/// elements and uses an ellipsis (...) for the rest.
///
/// The goal is to make the reading flow slightly better. For instance, if you have a vector of
/// subgraphs `s = ["A", "B", "C"]`, then `join_strings(s.iter(), Default::default())` will yield
/// "A, B and C".
pub(crate) fn join_strings(
    mut iter: impl Iterator<Item = impl AsRef<str>>,
    options: JoinStringsOptions,
) -> String {
    let mut output = String::new();
    let Some(first) = iter.next() else {
        return output;
    };
    output.push_str(first.as_ref());
    let Some(second) = iter.next() else {
        return output;
    };
    // PORT_NOTE: The analogous JS code in `printHumanReadableList()` was only tracking the length
    // of elements getting added to the list and ignored separators, so we do the same here.
    let mut element_length = first.as_ref().chars().count();
    // Returns true if push would exceed limit, and instead pushes default separator and "...".
    let mut push_sep_and_element = |sep: &str, element: &str| {
        if let Some(output_length_limit) = options.output_length_limit {
            // PORT_NOTE: The analogous JS code in `printHumanReadableList()` has a bug where it
            // doesn't early exit when the length would be too long, and later small elements in the
            // list may erroneously extend the printed subset. That bug is fixed here.
            let new_element_length = element_length + element.chars().count();
            return if new_element_length <= output_length_limit {
                element_length = new_element_length;
                output.push_str(sep);
                output.push_str(element);
                false
            } else {
                output.push_str(options.separator);
                output.push_str("...");
                true
            };
        }
        output.push_str(sep);
        output.push_str(element);
        false
    };
    let last_sep = options.last_separator.unwrap_or(options.separator);
    let Some(mut current) = iter.next() else {
        push_sep_and_element(options.first_separator.unwrap_or(last_sep), second.as_ref());
        return output;
    };
    if push_sep_and_element(
        options.first_separator.unwrap_or(options.separator),
        second.as_ref(),
    ) {
        return output;
    }
    for next in iter {
        if push_sep_and_element(options.separator, current.as_ref()) {
            return output;
        }
        current = next;
    }
    push_sep_and_element(last_sep, current.as_ref());
    output
}

pub(crate) struct HumanReadableListOptions<'a> {
    pub(crate) prefix: Option<HumanReadableListPrefix<'a>>,
    pub(crate) last_separator: Option<&'a str>,
    /// When displaying a list of something in a human-readable form, after what size (in number of
    /// characters) we start displaying only a subset of the list.
    pub(crate) output_length_limit: usize,
    /// If there are no elements, this string will be used instead.
    pub(crate) empty_output: &'a str,
}

pub(crate) struct HumanReadableListPrefix<'a> {
    pub(crate) singular: &'a str,
    pub(crate) plural: &'a str,
}

impl Default for HumanReadableListOptions<'_> {
    fn default() -> Self {
        Self {
            prefix: None,
            last_separator: Some(" and "),
            output_length_limit: 100,
            empty_output: "",
        }
    }
}

// PORT_NOTE: Named `printHumanReadableList` in the JS codebase, but "print" in Rust has the
// implication it prints to stdout/stderr, so we remove it here. Also, the "emptyValue" option is
// never used, so it's not ported.
/// Like [join_strings], joins an iterator of strings, but with a few differences, namely:
/// - It allows prefixing the whole list, and to use a different prefix if there's only a single
///   element in the list.
/// - It forces the use of ", " as separator, but allows a different last separator.
/// - It forces an output length limit to be specified. In other words, this function assumes it's
///   more useful to avoid flooding the output than printing everything when the list is too long.
pub(crate) fn human_readable_list(
    mut iter: impl Iterator<Item = impl AsRef<str>>,
    options: HumanReadableListOptions,
) -> String {
    let Some(first) = iter.next() else {
        return options.empty_output.to_owned();
    };
    let Some(second) = iter.next() else {
        return if let Some(prefix) = options.prefix {
            format!("{} {}", prefix.singular, first.as_ref())
        } else {
            first.as_ref().to_owned()
        };
    };
    let joined_strings = join_strings(
        [first, second].into_iter().chain(iter),
        JoinStringsOptions {
            last_separator: options.last_separator,
            output_length_limit: Some(options.output_length_limit),
            ..Default::default()
        },
    );
    if let Some(prefix) = options.prefix {
        format!("{} {}", prefix.plural, joined_strings)
    } else {
        joined_strings
    }
}

// PORT_NOTE: Named `printSubgraphNames` in the JS codebase, but "print" in Rust has the implication
// it prints to stdout/stderr, so we've renamed it here to `human_readable_subgraph_names`
pub(crate) fn human_readable_subgraph_names(
    subgraph_names: impl Iterator<Item = impl AsRef<str>>,
) -> String {
    human_readable_list(
        subgraph_names.map(|name| format!("\"{}\"", name.as_ref())),
        HumanReadableListOptions {
            prefix: Some(HumanReadableListPrefix {
                singular: "subgraph",
                plural: "subgraphs",
            }),
            ..Default::default()
        },
    )
}

// PORT_NOTE: Named `printTypes` in the JS codebase, but "print" in Rust has the implication
// it prints to stdout/stderr, so we've renamed it here to `human_readable_types`
pub(crate) fn human_readable_types(types: impl Iterator<Item = impl AsRef<str>>) -> String {
    human_readable_list(
        types.map(|t| format!("\"{}\"", t.as_ref())),
        HumanReadableListOptions {
            prefix: Some(HumanReadableListPrefix {
                singular: "type",
                plural: "types",
            }),
            ..Default::default()
        },
    )
}
