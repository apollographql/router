### Fix Router's validation of ObjectValue variables ([PR #8821](https://github.com/apollographql/router/pull/8821))

This change addresses an issue in Router whereby invalid additional fields of an input object were able to pass variable validation because the fields of the object were not being properly checked.

Example:
```
## schema ##
input MessageInput {
    content: String
    author: String
}
type Receipt {
    id: ID!
}
type Query{
    send(message: MessageInput): Receipt
}

## query ##
query($msg: MessageInput) {
    send(message: $msg) {
        id
    }
}

## input variables ##
{"msg":  
    {
    "content": "Hello",
    "author": "Me",
    "unknownField": "unknown",
    }
}
```
This request would pass validation because the variable `msg` from the query was present in the input, however, the fields of `msg` from the input were not being validated against the `MessageInput` type.

This change also adds unit tests.

<!-- [ROUTER-981] -->
---

**Checklist**

Complete the checklist (and note appropriate exceptions) before the PR is marked ready-for-review.

- [x] PR description explains the motivation for the change and relevant context for reviewing
- [x] PR description links appropriate GitHub/Jira tickets (creating when necessary)
- [x] Changeset is included for user-facing changes
- [x] Changes are compatible[^1]
- [ ] Documentation[^2] completed
- [ ] Performance impact assessed and acceptable
- [ ] Metrics and logs are added[^3] and documented
- Tests added and passing[^4]
    - [x] Unit tests
    - [ ] Integration tests
    - [ ] Manual tests, as necessary

**Exceptions**

*Note any exceptions here*

**Notes**

[^1]: It may be appropriate to bring upcoming changes to the attention of other (impacted) groups. Please endeavour to do this before seeking PR approval. The mechanism for doing this will vary considerably, so use your judgement as to how and when to do this.
[^2]: Configuration is an important part of many changes. Where applicable please try to document configuration examples.
[^3]: A lot of (if not most) features benefit from built-in observability and `debug`-level logs. Please read [this guidance](https://github.com/apollographql/router/blob/dev/dev-docs/metrics.md#adding-new-metrics) on metrics best-practices.
[^4]: Tick whichever testing boxes are applicable. If you are adding Manual Tests, please document the manual testing (extensively) in the Exceptions.


[ROUTER-981]: https://apollographql.atlassian.net/browse/ROUTER-981?atlOrigin=eyJpIjoiNWRkNTljNzYxNjVmNDY3MDlhMDU5Y2ZhYzA5YTRkZjUiLCJwIjoiZ2l0aHViLWNvbS1KU1cifQ

By [@conwuegb](https://github.com/conwuegb) in https://github.com/apollographql/router/pull/8821