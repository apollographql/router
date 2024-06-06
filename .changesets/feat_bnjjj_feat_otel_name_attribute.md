### Add the ability to override span name with otel.name attribute ([Issue #5261](https://github.com/apollographql/router/issues/5261))

It gives you the ability to override the span name by using custom telemetry with any [selectors](https://www.apollographql.com/docs/router/configuration/telemetry/instrumentation/selectors/) you want just by setting the `otel.name` attribute.

Example:

```yaml
telemetry:
  instrumentation:
    spans:
      router:
        otel.name:
           static: router # Override the span name to router 
```



---

**Checklist**

Complete the checklist (and note appropriate exceptions) before the PR is marked ready-for-review.

- [x] Changes are compatible[^1]
- [ ] Documentation[^2] completed
- [x] Performance impact assessed and acceptable
- Tests added and passing[^3]
    - [x] Unit Tests
    - [x] Integration Tests
    - [x] Manual Tests

**Exceptions**

*Note any exceptions here*

**Notes**

[^1]: It may be appropriate to bring upcoming changes to the attention of other (impacted) groups. Please endeavour to do this before seeking PR approval. The mechanism for doing this will vary considerably, so use your judgement as to how and when to do this.
[^2]: Configuration is an important part of many changes. Where applicable please try to document configuration examples.
[^3]: Tick whichever testing boxes are applicable. If you are adding Manual Tests, please document the manual testing (extensively) in the Exceptions.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/5365