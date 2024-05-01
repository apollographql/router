### Add gt operation for telemetry conditions ([PR #5048](https://github.com/apollographql/router/pull/5048))

Adds a greater than operator for telemetry conditions called `gt`. The configuration for `gt` takes two arguments as a list, similar to `eq`. The operator checks that the first argument is greater than the second. Other conditions such as `gte`, `lt`, `lte`, and `range` can all be made from combinations of `gt`, `eq`, and `all`.

<!-- ROUTER-237 -->
---

**Checklist**

Complete the checklist (and note appropriate exceptions) before the PR is marked ready-for-review.

- [X] Changes are compatible[^1]
- [ ] Documentation[^2] completed
- [ ] Performance impact assessed and acceptable
- Tests added and passing[^3]
    - [X] Unit Tests
    - [ ] Integration Tests
    - [ ] Manual Tests

**Exceptions**

*Note any exceptions here*

**Notes**

[^1]: It may be appropriate to bring upcoming changes to the attention of other (impacted) groups. Please endeavour to do this before seeking PR approval. The mechanism for doing this will vary considerably, so use your judgement as to how and when to do this.
[^2]: Configuration is an important part of many changes. Where applicable please try to document configuration examples.
[^3]: Tick whichever testing boxes are applicable. If you are adding Manual Tests, please document the manual testing (extensively) in the Exceptions.

By [@tninesling](https://github.com/tninesling) in https://github.com/apollographql/router/pull/5048
