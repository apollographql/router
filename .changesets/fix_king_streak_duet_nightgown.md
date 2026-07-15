### Value coercion and validation fixes

The `coerce_value()` function in `compat.rs` has been rewritten to fix multiple bugs in how default values in schemas and operations are coerced and validated.

Bug fixes:

- Invalid default values are now correctly reported as errors.
- Removed default value auto-expansion logic.
- Non-list value coercion is now only applied to operations.
- Fixed missing coercion edge cases to always reject null values applied to non-null types.
- Fixed validation of unknown fields in input object default values.
- Added missing enum value validations to ensure they are valid and are part of the enum definition.
- Adds missing validation for `@deprecated` on required arguments and input fields.

By [@sachindshinde](https://github.com/sachindshinde)
