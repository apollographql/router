### Federation v2.7.5 ([PR #5064](https://github.com/apollographql/router/pull/5064))

This brings in a query planner fix released in v2.7.5 of Apollo Federation.  Notably, from [its changelog](https://github.com/apollographql/federation/releases/tag/%40apollo%2Fquery-planner%402.7.5):

-   Fix issue with missing fragment definitions due to `generateQueryFragments`. ([#2993](https://github.com/apollographql/federation/pull/2993))

    An incorrect implementation detail in `generateQueryFragments` caused certain queries to be missing fragment definitions, causing the operation to be invalid and fail early in the request life-cycle (before execution). Specifically, subsequent fragment "candidates" with the same type condition and the same length of selections as a previous fragment weren't correctly added to the list of fragments. An example of an affected query is:

    ```graphql
    query {
      t {
        ... on A {
          x
          y
        }
      }
      t2 {
        ... on A {
          y
          z
        }
      }
    }
    ```

    In this case, the second selection set would be converted to an inline fragment spread to subgraph fetches, but the fragment definition would be missing
By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/5064