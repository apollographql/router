### Decide CSRF, enhanced client awareness and content negotiation without a copy of the stack

The CSRF check, enhanced client awareness and the router's content negotiation each cloned the stack beneath them and boxed a future on every request, though none of their decisions waits on anything. They now decide where the request is called and call the stack beneath in place. No answer changes.

By [@martijnwalraven](https://github.com/martijnwalraven)
