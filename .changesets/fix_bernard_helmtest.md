### helm test always fails

When running helm test it always generates an error because wget expects a 200 response, however calling a graphql endpoint without the proper body will return a 400 which is causing the test to fail. Using netcat (nc) will check that the port is up and return sucess once the router is working instead.

By [@bbardawilwiser](https://github.com/bbardawilwiser) in https://github.com/apollographql/router/pull/3096
