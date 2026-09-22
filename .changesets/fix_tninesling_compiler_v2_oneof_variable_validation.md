### Fix `@oneOf` null check when unknown fields are present ([PR #10119](https://github.com/apollographql/router/pull/10119))

The `@oneOf` variable validation null check inspected the first field in iteration order rather than the first recognized field. When `strict_variable_validation` was in measure mode and an unknown field preceded the recognized field, the null check could inspect the wrong value and incorrectly pass validation.

By [@tninesling](https://github.com/tninesling) in https://github.com/apollographql/router/pull/10119
