### docs: clarify coprocessor router stage responses ([PR #4189](https://github.com/apollographql/router/pull/4189))

The coprocessor RouterRequest and RouterResponse stages fully support `control: { break: 500 }` but the response body must be a string. This change provides examples in the "Terminating a client request" section.


<!-- start metadata -->
---

By [@lennyburdette](https://github.com/lennyburdette) in https://github.com/apollographql/router/pull/4189