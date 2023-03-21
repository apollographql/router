### panicking MockService 

The test code we have to write is complicated due to the requirements that we clone services. To avoid that, we tried to use the service mocking from tower_test, but it is not compatible with mockall: mockall panics when we don't match its expectations, but tower_test services stop panics at the task boundary.

This introduces a new MockService type that catches panics, transforms them into errors, then make them panic again, but in the context of the service caller

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/2275