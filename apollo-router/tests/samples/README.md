# File based integration tests

This folder contains a series of Router integration tests that can be defined entirely through a JSON file. Thos tests are able to start and stop a router, reload its schema or configiration, make requests and check the expected response. While we can make similar tests from inside the Router's code, these tests here are faster to write and modify because they do not require recompilations of the Router, at the cost of a slightly higher runtime cost.

## How to write a test

One test is recognized as a folder containing a `plan.json` file. Any number of subfolders is accepted, and the test name will be the path to the test folder. If the folder contains a `README.md` file, it will be added to the captured output of the test, and displayed if the test failed.

The `plan.json` file contains a top level JSON object with an `actions` field, containing an array of possible actions, that will be executed one by one:

```json
{
    "enterprise": false,
    "actions": [
        {
            "type": "Start",
            "schema_path": "./supergraph.graphql",
            "configuration_path": "./configuration.yaml",
            "subgraphs": {}
        },
        {
            "type": "Request",
            "request": {
                "query": "{ me { name } }"
            },
            "expected_response": {
                "data":{
                    "me":{
                        "name":"Ada Lovelace"
                    }
                }
            }
        },
        {
            "type": "Stop"
        }
    ]
}
```

If any of those actions fails, the test will stop immediately.

The `enterprise` field indicates that this test uses enterprise features. If the `TEST_APOLLO_KEY` and `TEST_APOLLO_GRAPH_REF` environment variables are present with valid values, the test will be executed. Otherwise, it will be skipped.

## Possible actions

### Start

```json
{
    "type": "Start",
    "schema_path": "./supergraph.graphql",
    "configuration_path": "./configuration.yaml",
    "subgraphs": {
        "accounts": {
            "requests": [
                {
                    "request": {"query":"{me{name}}"},
                    "response": {"data": { "me": { "name": "test" } } }
                },
                {
                    "request": {"query":"{me{nom:name}}"},
                    "response": {"data": { "me": { "nom": "test" } } }
                }
            ]
        }
    }
}
```

the `schema_path` and `configuration_path` field are relative to the test's folder. The `subgraph` field can contain mocked requests and responses for each subgraph. If the Router fails to load with this schema and configuration, then this action will fail the test.

## Reload configuration

Reloads the router with a new configuration file. If the Router fails to load the new configuration, then this action will fail the test.

```json
{
    "type": "ReloadConfiguration",
    "configuration_path": "./configuration.yaml"
}
```

## Reload schema

Reloads the router with a new schema file. If the Router fails to load the new configuration, then this action will fail the test.

```json
{
    "type": "ReloadSchema",
    "schema_path": "./supergraph.graphql"
}
```

## Request

Sends a request to the Router, and verifies that the response body matches the expected response. If it does not match or returned any HTTP error, then this action will fail the test.
```json
{
    "type": "Request",
    "request": {
        "query": "{ me { name } }"
    },
    "expected_response": {
        "data":{
            "me":{
                "name":"Ada Lovelace"
            }
        }
    }
}
```

### Stop

Stops the Router. If the Router does not stop correctly, then this action will fail the test.

```json
{
    "type": "Stop"
}
```