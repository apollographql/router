### Fix compatibility of coprocessor metric creation ([PR #4930](https://github.com/apollographql/router/pull/4930))

Previously, the router's execution stage created coprocessor metrics differently than other stages. This produced metrics with slight incompatibilities. 

This release fixes the issue by creating coprocessor metrics in the same way as all other stages.  

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/4930