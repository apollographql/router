# Yaml config design

The router uses yaml configuration, and when creating new features or extending existing features you'll likely need to think about how configuration is exposed.

In general users should have a pretty good idea of what a configuration option does without referring to the documentation.

## Migrations

We won't always get things right, and sometimes we'll need to provide [migrations](apollo-router/src/configuration/migrations/README.md) from old config to new config.

Make sure you:
1. Mention the change in the changelog
2. Update docs
3. Update any test configuration
4. Create a migration test as detailed in [migrations](apollo-router/src/configuration/migrations/README.md)
5. In your migration description tell the users what they have to update.

## Process
It should be obvious to the user what they are configuring and how it will affect Router behaviour. It's tricky for us as developers to know when something isn't obvious to users as often we are too close to the domain.

Complex config should be run by the rest of the team. Ideally before writing any code, as coming to the team late can cause code churn and frustration. The process is as follows:
1. In the github issue put the proposed config in.
2. List any concerns.
3. Notify the team that you are looking for request for comment.
4. Schedule a meeting to discuss. (This is important, often design considerations will fall out of conversation)
5. If it is not completely clear what the direction should be:
1. Wait a few days, often people will have ideas later even if they didn't in the meeting.
2. Ask users what they think.
6. Make your changes.

Note that these are not hard and fast rules, and if your config is really obviously correct then bu all means make the change and be prepared to deal with comments at the review stage.

## Design patterns

Use the following as a rule of thumb, also look at existing config for inspiration.
The most important goal is usability, so do break the rules if it makes sense, but it's worth bringing the discussion to the team in such circumstances.  

1. [Avoid empty config](#avoid-empty-config).
2. [Do use `#[serde(deny_unknown_fields)]`](#do-use-serdedeny_unknown_fields).
3. [Don't use `#[serde(flatten)]`](#dont-use-serdeflatten).
4. [Use consistent terminology](#use-consistent-terminology).
5. [Document your configuration options](#document-your-configuration-options).
6. [Plan for the future](#plan-for-the-future).

### Avoid empty config

In Rust you can use `Option` to say that config is optional, however this can give a bad experience if the type is complex and all fields are optional.

#### GOOD
```rust
#[serde(deny_unknown_fields)]
struct Export {
    url: Url // url is required
}
```
```yaml
export:
    url: http://example.com
```

#### GOOD
```rust
enum ExportUrl {
    Default,
    Url(Url)
}

#[serde(deny_unknown_fields)]
struct Export {
    url: ExportUrl // Url is required but user may specify `default`
}
```
```yaml
export:
    url: default
```

#### BAD
```rust
#[serde(deny_unknown_fields)]
struct Export {
    url: Optional<Url> // url is optional
}
```
```yaml
export: # The user is not aware that url was defaulted.
```

#### UGLY
In the case where you genuinely have no config then it may be acceptable to have an `enabled: bool` flag.
The reason to avoid this is that it creates a disconnect between the user and the thing that they are trying to do
```rust
#[serde(deny_unknown_fields)]
struct Export {
    enabled: bool,
    url: Optional<Url> // url is optional
}
```
```yaml
export: 
  enabled: true # The user is not aware that url was defaulted.
```

### Do use `#[serde(deny_unknown_fields)]`.
Every container that takes part in config should be annotated with `#[serde(deny_unknown_fields)]`. If not the user can make mistakes on their config and they they won't get errors.

#### GOOD
```rust
#[serde(deny_unknown_fields)]
struct Export {
    url: Url
}
```
```yaml
export: 
  url: http://example.com
  backup: http://example2.com # The user will receive an error for this
```

#### BAD
```rust
struct Export {
    url: Url
}
```
```yaml
export: 
  url: http://example.com
  backup: http://example2.com # The user will NOT receive an error for this
```

### Don't use `#[serde(flatten)]`
Serde flatten is tempting to use where you have identified common functionality, but creates a bad user experience as it is incompatible with `#[serde(deny_unknown_fields)]`. There isn't a great solution to this, but you can use a macro to make things dry.

See [serde documentation](https://serde.rs/field-attrs.html#flatten) for more details.

#### UGLY
```rust
#[serde(deny_unknown_fields)]
struct Export {
    common_export_fields!()
}
```
```yaml
export: 
  url: http://example.com
  backup: http://example2.com
```

#### BAD
```rust
#[serde(deny_unknown_fields)]
struct Export {
    #[serde(flatten)]
    export_fields: CommonExportFields
}
```
```yaml
export: 
  url: http://example.com
  backup: http://example2.com # The user will NOT receive an error for this
```

### Use consistent terminology
Be consistent with the rust API terminology.
* request - functionality that modifies the request or retrieves data from the request of a service.
* response - functionality that modifies the response or retrieves data from the response of a service.
* supergraph - functionality within Plugin::subgraph_service
* execution - functionality within Plugin::execution_service
* subgraph(s) - functionality within Plugin::subgraph_service

If you use the above terminology then changes are you are doing something that will take place on every request. In this case make sure to include an `action` verb so the user know what the config is doing.

#### GOOD
```yaml
headers:
  subgraphs: # Modifies the subgraph service  
    products: 
      request: # Retrieves data from the request
        - propagate: # The action.
            named: foo
```

#### BAD
```yaml
headers:
  named: foo # From where, what are we doing, when is it happening?
```

### Document your configuration options
If your config is well documented in Rust then it will be well documented in the generated Json Schema. This means that when users are modifying their config either in their IDE or in Apollo GraphOS documentation is available.

Example configuration should be included on all containers.

#### GOOD
```rust
/// Export the data to the metrics endpoint
/// Example configuration:
/// ```yaml
/// export:
///   url: http://example.com
/// ```
#[serde(deny_unknown_fields)]
struct Export {
    /// The url to export metrics to.
    url: Url
}
```

#### BAD
```rust
#[serde(deny_unknown_fields)]
struct Export {
    url: Url
}
```

In addition, make sure to update the html documentation. 

### Don't leak config
There are exceptions, but in general config should not be leaked from plugins. By reaching into a plugin config from outside of a plugin, there is leakage of functionality outside of compilation units.

For Routers where the `Plugin` trait does not yet have `http_service` there will be leakage of config. The addition of the `http_service` to `Plugin` should eliminate the need to leak config.  

### Plan for the future

Often configuration will be limited initially as a feature will be developed over time. It's important to consider what may be added in future.

Examples of things that typically require extending later:
* Connection info to other systems.
* An action that retrieves information from a domain object e.g. `request.body`, `request.header`

Often adding container objects can

#### GOOD
```rust
#[serde(deny_unknown_fields)]
struct Export {
    url: Url
    // Future export options may be added here
}
#[serde(deny_unknown_fields)]
struct Telemetry {
    export: Export
}
```
```yaml
telemetry:
  export: 
    url: http://example.com
```
#### BAD
```rust
#[serde(deny_unknown_fields)]
struct Telemetry {
    url: Url   
}
```
```yaml
telemetry:
  url: http://example.com # Url for what? 
```
#### BAD
```rust
#[serde(deny_unknown_fields)]
struct Telemetry {
    export_url: Url // export_url is not extendable. You can't add things like auth. 
}
```
```yaml
telemetry:
  export_url: http://example.com # How do I specify auth
```


