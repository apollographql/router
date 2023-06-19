### update Rhai to 1.15.0 to fix issue with hanging example test ([Issue #3213](https://github.com/apollographql/router/issues/3213))

One of our Rhai examples' tests have been regularly hanging in the CI builds for the last couple of months. Investigation uncovered a race condition within Rhai itself. This update brings in the fixed version of Rhai and should eliminate the hanging problem and improve build stability.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/3273