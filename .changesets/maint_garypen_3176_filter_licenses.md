### Improve `cargo-about` license checking ([Issue #3176](https://github.com/apollographql/router/issues/3176))

From the description of this [cargo about PR](https://github.com/EmbarkStudios/cargo-about/pull/216), it is possible for "NOASSERTION" identifiers to be added when gathering license information, causing license checks to fail. This change uses the new `cargo-about` configuration `filter-noassertion` to eliminate the problem.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/3178