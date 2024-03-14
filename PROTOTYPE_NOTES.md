# Subgraph Batching Prototype
Version: Draft 0.1

These notes are designed to maximise the utility of evaluating the subgraph batching prototype both for the customers involved and for Apollo.

Please read these notes carefully before:
 - Using the prototype router 
 - Reporting results back to Apollo

The [subgraph batching design document](https://docs.google.com/document/d/1KlGgNCm1sQWc-tYs2oqHauMBIYdrtlpIELySZDAnISE/edit?usp=sharing) may be helpful in evaluating the prototype.

## Using the prototype

### Deployment

#### Accessing the prototype

You can choose to build from source or from one of our pre-built options.

##### Source

The prototype router is publicly available on the main router repo on branch `subgraph-batching-prototype`. It can be accessed and built as normal.

##### Binary

If you want to use a pre-built router, you can access the prototype assets as follows:
| Artifact | Location |
| -------- | -------- |
| Architecture specific binaries | [REPLACE ME] |
| Docker image | [REPLACE ME] |
| Helm chart | [REPLACE ME] |

> Note: These assets are only available until [REPLACE ME]

### Configuration

Because this is a prototype, we can't update the main `router` documentation and we don't have a way to deliver documentation for prototypes. For this prototype these configuration notes should suffice.

The prototype is based off the latest version of the router so non subgraph batching specific documentation is [as usual](https://www.apollographql.com/docs/router/)

Subgraph batching may be enabled for either all subgraphs or named subgraphs as follows.

#### All Subgraphs

This snippet enables client-side and subgraph batching for all subgraphs.

```yaml
batching:
  enabled: true
  mode: batch_http_link
  subgraph:
    all:
      enabled: true
```
> Note: both `batching` and `batching.subgraph.all` must be enabled. `batching` enables client-side batching and everything under the optional `subgraph` key configures subgraph batching.

#### Names Subgraphs

This snippet enables client-side and subgraph batching for only the name `accounts` subgraph.

```yaml
batching:
  enabled: true
  mode: batch_http_link
  subgraph:
    subgraphs:
      accounts:
        enabled: true
```
Note: `batching` must be enabled and `subgraphs` is a list of named subgraphs which must be enabled. `batching` enables client-side batching and everything under the optional `subgraph` key configures subgraph batching. If a subgraph is not named (i.e.: not present in the sbugraphs list) then batching is not enabled for that subgraph. A subgraph may also be present but if `enabled` is `false`, that will also disable batching for that subgraph.

### Operation

During operation the prototype should behave like the latest release of the router. The following subgraph batching specific items will be of interest.

#### Metrics

The existing batching metrics, which are currently client-side batching specific, are enhanced with an optional `subgraph` attribute. If this attribute is not present, then the metrics relate to client side batching. If it is present, then the attribute will be the name of a subgraph and the metrics will relate to the named subgraph.

#### Tracing/Logging

[REPLACE ME]

## Reporting Results

There are a number of things we are interested in receiving feedback on. We are categorising these issues as major/minor within the context of the prototype. Of course, all issues are important, but some will require more engineering re-working (major) whereas others are more likely to be easy to address before the project completes (minor). Anyway, the advice about classification is just that, so feel free to report an issue as major or minor. :)

Please only report issues in the prototype back to your account team in this format. Don't use GitHub to file issues as we'll find it harder to track and address them in the prototype.

Once you have finished evaluating the prototype, please let us know the following:

### Major Issues

Any major issues: router hangs, panics, missing or incorrect data, performance degradation, ...

### Minor Issues

Any minor issues: increased resource consumption, inaccurate metrics, confusing configuration, missing tracing details, ...

Please provide as much detail as possible for each issue you report.

Feel free to provide multiple reports, but take some time to evaluate the prototype before reporting your first report. This will reduce overhead for all concerned.

# Sample Report

Customer: Starstuff

Contacts: engineer@starstuff.com, devops@starstuff.com

Batching Configuration Snippet:
```yaml
batching:
  enabled: true
  mode: batch_http_link
  subgraph:
    all:
      enabled: true
```
Studio Graph/Variant Name (if applicable): starstuff@prod

Major Issues

1. When executing `ListCustomers` operation the router hangs without returning to the client.
2. During execution of `ListOrders` the router panics with the following error message: `panicked during batch assembly`.
3. How can I confirm that subgraph batching is working as designed?

Minor Issues

1. The router is now consuming more memory with subgraph batching enabled. Previously: 150Mb, Now: 175Mb.
2. I can't tell from tracing records what the impact of subgraph batching is on traffic to my `accounts` subgraph.
