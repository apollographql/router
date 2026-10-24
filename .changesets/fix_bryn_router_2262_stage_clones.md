### Use less CPU per request in the request pipeline

Router 3.x pre-releases spent noticeably more CPU on each request than router 2.x, because handling a request repeatedly copied large parts of the router's request pipeline. The router now shares that pipeline between requests instead of copying it, so each request uses less CPU. You don't need to change anything to get the improvement.

By [@BrynCooke](https://github.com/BrynCooke)
