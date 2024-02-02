### Add a new selector to get all baggage key values in span attributes ([Issue #4425](https://github.com/apollographql/router/issues/4425))

If you have several baggage items and would like to add all of them directly as span attributes, for example `baggage: my_item=test, my_second_item=bar` will add 2 span attributes `my_item=test` and `my_second_item=bar`.

Example of configuration:

```yaml
telemetry:
  instrumentation:
    spans:
      router:
        attributes:
          baggage: true
```



**Checklist**

Complete the checklist (and note appropriate exceptions) before the PR is marked ready-for-review.

- [x] Changes are compatible[^1]
- [x] Documentation[^2] completed
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

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/4537