### Actually replace the old query planner on reload 

The new query planner reload code introduced a regression where new schemas were not used, because the old query planner instance was kept instead of the new one.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/2895