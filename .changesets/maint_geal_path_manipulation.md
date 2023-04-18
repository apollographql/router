### lighter path manipulation in response formatting 

Response formatting generates a lot of temporary allocations to create response paths that end up unused. By making a reference based type to hold these paths, we can prevent those allocations and improve performance.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/2854