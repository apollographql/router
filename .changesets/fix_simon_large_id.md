### Fix validation error with ID variable values overflowing i32 ([Issue #3873](https://github.com/apollographql/router/issues/3873))

Input values for variables of type `ID` were previously validated as "either like a GraphQL `Int` or like a GraphQL `String`". GraphQL `Int` is specified as a signed 32-bit integer, such that values that overflow fail validation. Applying this range restriction to `ID` values was incorrect. Instead, validation for `ID` now accepts any JSON integer or JSON string value, so that IDs larger than 32 bits can be used.

By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/3896
