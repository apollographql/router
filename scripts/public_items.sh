#!/bin/bash
#
# This script compiles the `apollo-router` crate,
# uses rustdoc’s JSON output to extract public API items,
# then uses jq to format as Rust `use` imports ready to be copy-pasted.
#
# The output is lengthy, consider using a pager:
#
#     scripts/public_items.sh | less


cd $(dirname $0)/..

cargo +nightly rustdoc --lib -p apollo-router -- \
  -Z unstable-options --output-format json

< target/doc/apollo_router.json jq -r '
    [
        .paths[]
        | select(
                .kind != "module" 
            and .kind != "variant"
        )
        | .path
        | select(.[0] == "apollo_router")
        | join("::")
        | "use " + . + ";"
    ]
    | sort
    | .[]
'
