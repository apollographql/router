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

The prototype router is publicly available on the main router repo on branch `preview-0-subgraph-batching`. It can be accessed and built as normal.

##### Binary

If you want to use a pre-built router, you can access the prototype assets as follows:
| Artifact |
| -------- |
| [macOS x86](https://output.circle-artifacts.com/output/job/b535bcd5-61b4-485c-b6ed-3d10b38e18bb/artifacts/0/artifacts/router-v0.0.0-nightly.20240314+a224e725-x86_64-apple-darwin.tar.gz)
| [macOS Arm](https://output.circle-artifacts.com/output/job/e2c86a66-0f85-49ce-aa53-a0832ee62d24/artifacts/0/artifacts/router-v0.0.0-nightly.20240314+a224e725-aarch64-apple-darwin.tar.gz)
| [Windows](https://app.circleci.com/pipelines/github/apollographql/router/19665/workflows/6f35a964-a552-4050-9820-354e418286ce/jobs/131264/artifacts#:~:text=artifacts/router%2Dv0.0.0%2Dnightly.20240314%2Ba224e725%2Dx86_64%2Dpc%2Dwindows%2Dmsvc.tar.gz)
| [Linux x86](https://output.circle-artifacts.com/output/job/0317ca69-2ec3-428a-a72f-a131e747eb1f/artifacts/0/artifacts/router-v0.0.0-nightly.20240314+a224e725-x86_64-unknown-linux-gnu.tar.gz)
| [Linux Arm](https://output.circle-artifacts.com/output/job/5ef910c0-2c68-41f7-8760-ef93bfd633ce/artifacts/0/artifacts/router-v0.0.0-nightly.20240314+a224e725-aarch64-unknown-linux-gnu.tar.gz)
| [Docker image](https://github.com/apollographql/router/pkgs/container/nightly%2Frouter/190888163?tag=v0.0.0-nightly.20240314-a224e725)
| [Docker Debug image](https://github.com/apollographql/router/pkgs/container/nightly%2Frouter/190887938?tag=v0.0.0-nightly.20240314-a224e725-debug)
| [Helm chart](https://github.com/apollographql/router/pkgs/container/helm-charts-nightly%2Frouter/190888427?tag=0.0.0-nightly.20240314-a224e725)

> Note: These assets are only available until 4/25/2024.

### Configuration

For testing this prototype these configuration notes should suffice.

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

#### Named Subgraphs

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
> Note: `batching` must be enabled and `subgraphs` is a list of named subgraphs which must be enabled. `batching` enables client-side batching and everything under the optional `subgraph` key configures subgraph batching. If a subgraph is not named (i.e., not present in the subgraphs list), then batching is not enabled for that subgraph. A subgraph may also be present, but if `enabled` is `false`, that will also disable batching for that subgraph.

### Operation

During operation the prototype should behave like the latest release of the router. The following subgraph batching specific items will be of interest.

#### Metrics

The existing batching metrics, which are currently client-side batching specific, are enhanced with an optional `subgraph` attribute. If this attribute is not present, then the metrics relate to client side batching. If it is present, then the attribute will be the name of a subgraph and the metrics will relate to the named subgraph.

#### Tracing/Logging

#### Subgraph Request Span

This span has an attribute named: `graphql.operation.name`. Usually, for non-batched operations, this will contain the operation name for the request. However, for a batch of operations, there isn't a single operation name, for multiple operations are being made in the same request. We have taken the decision to set the `graphql.operation.name` attribute to "batch". Let us know if you have alternative suggestions or would like to see this treated a different way.

#### Additional Logging

Because this is prototype code, you'll see various logging statement at INFO level about the operation of the batching code. We'll either remove or lower the level of these log statements in the final release, so don't be concerned about these additional logs.

The additional logs will mention "waiters" or "subgraph fetches" and may be useful when reporting issues to us.

## Reporting Results

There are a number of things we are interested in receiving feedback on. We are categorising these issues as major/minor within the context of the prototype. Of course, all issues are important, but some will require more engineering re-working (major) whereas others are more likely to be easy to address before the project completes (minor). Anyway, the advice about classification is just that, so feel free to report an issue as major or minor. :)

Please only report issues in the prototype back to your account team in this format. Don't use GitHub to file issues as we'll find it harder to track and address them in the prototype.

Once you have finished evaluating the prototype, please let us know the following:

### Major Issues

Any major issues: router hangs, panics, missing or incorrect data, performance degradation, ...

### Minor Issues

Any minor issues: increased resource consumption, inaccurate metrics, confusing configuration, missing tracing details, ...

Please provide as much detail as possible for each issue you report. Please include whether you tested a router built from source or one of our pre-built binaries.

Feel free to provide multiple reports, but take some time to evaluate the prototype before reporting your first report. This will reduce overhead for all concerned.

# Sample Report

Customer: Starstuff

Contacts: engineer@starstuff.com, devops@starstuff.com

Evaluated: macOS Arm

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
4. The performance of non-batch operations is impacted with this prototype.

Minor Issues

1. The router is now consuming more memory with subgraph batching enabled. Previously: 150Mb, Now: 175Mb.
2. I can't tell from tracing records what the impact of subgraph batching is on traffic to my `accounts` subgraph.
