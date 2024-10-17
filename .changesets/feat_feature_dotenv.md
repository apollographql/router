### Add dotenv to allow router to read environment variables from .env file ([PR #6117](https://github.com/apollographql/router/pull/6117))

Router will now read environment variables from `.env` file. This is helpful for local development to inject in `APOLLO_KEY` and `APOLLO_GRAPH_REF` from a `.env` file. It also makes these environment variables available everywhere they are currently available such as in rhai scripts:

```
# .env
MY_COOL_VARIABLE="yeaaaaa man!"

# main.rhai
log_info(`MY_COOL_VARIABLE: ${env::get("MY_COOL_VARIABLE")}`);
```

By [@andrewmcgivery](https://github.com/andrewmcgivery) in https://github.com/apollographql/router/pull/6117
