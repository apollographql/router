### Copy less of the response while executing query plans ([PR #PULL_NUMBER](https://github.com/apollographql/router/pull/PULL_NUMBER))

While executing a query plan, the router copied the response built so far at several points, most of all when a sequence of fetches ran alongside other fetches. Each copy grew with the size of the response. The router now keeps only the data each fetch adds, shares each fetch's details between steps instead of copying them, and does less work per field when formatting the response.

In a benchmark of a query over 1,000 products whose plan runs a sequence of fetches in parallel with another fetch, each request takes about 11% less time.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/PULL_NUMBER
