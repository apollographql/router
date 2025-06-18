### Improved router code organization for subgraph HTTP communication ([PR #7692](https://github.com/apollographql/router/pull/7692))

This change improves the internal organization of the Apollo Router's subgraph communication code by extracting HTTP-related utilities into dedicated modules. This refactoring makes the codebase more maintainable and sets the foundation for future HTTP protocol enhancements.

**What changed for users:**
- No functional changes - all router behavior remains identical
- No configuration changes required
- No API changes

**Internal improvements:**
- Better separation of HTTP client utilities from core subgraph service logic
- Improved code organization and testability
- Foundation for future HTTP communication enhancements

This is the first step in a series of code organization improvements to the subgraph service architecture.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/7692
