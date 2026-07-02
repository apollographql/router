### Fix composition field merging when subtyping ([PR #9751](https://github.com/apollographql/router/pull/9751))

When composition merges fields with different return types, it was previously allowing nullable types to be considered subtypes of non-null supertypes. The resulting supergraph schema could cause query plan execution to error if the subgraph returns null at runtime. This bug has been fixed, and composition will now appropriately error.

By [@sachindshinde](https://github.com/sachindshinde) in https://github.com/apollographql/router/pull/9751
