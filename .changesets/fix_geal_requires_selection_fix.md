### Fix requires selection in arrays ([Issue #3972](https://github.com/apollographql/router/issues/3972))

When a field has a `@requires` annotation that selects an array, and some fields are missing in that array or some of the elements are null, the router would short circuit the selection and remove the entire array. This relaxes the condition to allow nulls in the selected array

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/3975