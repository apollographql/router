### Improved file uploads plugin architecture ([PR #7693](https://github.com/apollographql/router/pull/7693))

The file uploads plugin has been refactored to use a more robust and consistent architecture. This internal improvement enhances the reliability and maintainability of file upload handling without changing any user-facing functionality.

**What changed:**
- File uploads now use a proper HTTP service layer instead of a temporary workaround
- Better integration with the router's service pipeline
- Improved code organization and consistency with other plugins

**Impact:**
- No changes to file upload configuration or behavior
- No breaking changes to existing file upload functionality  
- Enhanced reliability and performance consistency
- Better foundation for future file upload improvements

All existing file upload features continue to work exactly as before.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/7693
