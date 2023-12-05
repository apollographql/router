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

Complex configuration changes should be discussed with the team before starting the implementation, since they will drive the code's design. The process is as follows:
1. In the github issue put the proposed config in.
2. List any concerns.
3. Notify the team that you are looking for request for comment.
4. Ask users what they think.
5. If you are an Apollo Router team member then schedule a meeting to discuss. (This is important, often design considerations will fall out of conversation)
6. If it is not completely clear what the direction should be:
7. Wait a few days, often people will have ideas later even if they didn't in the meeting.
8. Make your changes.

Note that these are not hard and fast rules, and if your config is really obviously correct then by all means make the change and be prepared to deal with comments at the review stage.

## Design patterns

Use the following as a rule of thumb, also look at existing config for inspiration.
The most important goal is usability, so do break the rules if it makes sense, but it's worth bringing the discussion to the team in such circumstances.  

1. [Avoid empty config](#avoid-empty-config).
2. [Use `#[serde(default)]`](#use-serdedefault).
3. [Use `#[serde(default)]` on struct instead of fields when possible](#use-serdedefault-on-struct-instead-of-fields-when-possible).
4. [Avoid `Option<...>`](#avoid-option)
5. [Do use `#[serde(deny_unknown_fields)]`](#use-serdedenyunknownfields).
6. [Don't use `#[serde(flatten)]`](#dont-use-serdeflatten).
7. [Use consistent terminology](#use-consistent-terminology).
8. [Don't use negative options](#dont-use-negative-options).
9. [Document your configuration options](#document-your-configuration-options).
10. [Plan for the future](#plan-for-the-future).
11. [General naming](#general-naming-guidelines).

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

#### GOOD
In the case where you genuinely have no config or all sub-options have obvious defaults then use an `enabled: bool` flag.
```rust
#[serde(deny_unknown_fields)]
struct Export {
    enabled: bool,
    #[serde(default = "default_resource")]
    url: Url // url is optional, see also but see advice on defaults.
}
```
```yaml
export: 
  enabled: true 
```

#### BAD
```rust
#[serde(deny_unknown_fields)]
struct Export {
    url: Option<Url>
}
```
```yaml
export: # The user is not aware that url was defaulted.
```

### Use `#[serde(default)]`
`#[serde(default="default_value_fn")` can be used to give fields defaults, and using this means that a generated json schema will also contain those defaults. The result of a default fn should be static.

#### GOOD
```rust
#[serde(deny_unknown_fields)]
struct Export {
    #[serde(default="default_url_fn")
    url: Url
}
```

#### BAD
This could leak a password into a generated schema.
```rust
#[serde(deny_unknown_fields)]
struct Export {
    #[serde(default="password_from_env_fn")
    password: String 
}
```
Take a look at `env_defaults` in `expansion.rs` to see how env variables should be defaulted.

### Use `#[serde(default)]` on struct instead of fields when possible
If all the fields of your struct have their default value then use the `#[serde(default)]` on the struct instead of all fields. If you have specific default values for field, you have to create your own `Default` impl. By doing this we will have the same behavior if we're deserializing the struct and if we create it with the `Default` implementation, it uses the same mechanism not only a specific mechanism for `serde`.

#### GOOD
```rust
#[serde(deny_unknown_fields, default)]
struct Export {
    url: Url,
    enabled: bool
}

impl Default for Export {
  fn default() -> Self {
    Self {
      url: default_url_fn(),
      enabled: false
    }
  }
}
```

#### BAD
```rust
#[serde(deny_unknown_fields)]
struct Export {
    #[serde(default="default_url_fn")
    url: Url,
    #[serde(default)]
    enabled: bool
}
```

### Avoid `Option<...>`
Using option significantly complicates consuming code as it has to deal with the presence or otherwise, especially where you are transferring data to a non-buildstructor builder where `with_` methods are not present.

#### GOOD
```rust
#[serde(deny_unknown_fields)]
struct Export {
    #[serde(default="default_url_fn")
    url: Url
}
```
```rust
builder = builder.with_url(config.url);
```

#### BAD
```rust
#[serde(deny_unknown_fields)]
struct Export {
    url: Option<Url>
}
```
```rust
if let Some(url) = config.url {
    builder = builder.with_url(url);
}
```


### Use `#[serde(deny_unknown_fields)]`
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
Serde flatten is tempting to use where you have identified common functionality, but creates a bad user experience as it is incompatible with `#[serde(deny_unknown_fields)]`. There isn't a great solution to this, but nesting config can sometimes help.

See [serde documentation](https://serde.rs/field-attrs.html#flatten) for more details.

#### MAYBE
```rust
#[serde(deny_unknown_fields)]
struct Export {
    url: Url,
    backup: Url
}
#[serde(deny_unknown_fields)]
struct Telemetry {
    export: Export
}
#[serde(deny_unknown_fields)]
struct Metrics {
    export: Export
}
```
```yaml
telemetry:
  export: 
    url: http://example.com
    backup: http://example2.com
metrics:
  export:
    url: http://example.com
    backup: http://example2.com
```

#### BAD
```rust
#[serde(deny_unknown_fields)]
struct Export {
    url: Url,
    backup: Url
}
struct Telemetry {
    export: Export
}
```
```yaml
telemetry: 
  url: http://example.com
  backup: http://example2.com
  unknown: sadness # The user will NOT receive an error for this
```

### Use consistent terminology
Be consistent with the rust API terminology.
* request - functionality that modifies the request or retrieves data from the request of a service.
* response - functionality that modifies the response or retrieves data from the response of a service.
* supergraph - functionality within Plugin::supergraph_service
* execution - functionality within Plugin::execution_service
* subgraph(s) - functionality within Plugin::subgraph_service

If you use the above terminology then chances are you are doing something that will take place on every request. In this case make sure to include an `action` verb so the user know what the config is doing.

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

### Don't use negative options

Router config uses positive options with defaults, this way users don't have to do the negation when reading the config. 

#### GOOD
```yaml
homepage:
  enabled: true
  log_headers: true
```

#### BAD
```yaml
my_plugin:
  disabled: false 
  redact_headers: false
```


### Document your configuration options
If your config is well documented in Rust then it will be well documented in the generated JSON Schema. This means that when users are modifying their config either in their IDE or in Apollo GraphOS, documentation is available.

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

In addition, make sure to update the published documentation in the `docs/` folder.

### Don't leak config
There are exceptions, but in general config should not be leaked from plugins. By reaching into a plugin config from outside of a plugin, there is leakage of functionality outside of compilation units.

For Routers where the `Plugin` trait does not yet have `http_service` there will be leakage of config. The addition of the `http_service` to `Plugin` should eliminate the need to leak config.  

### Plan for the future

Often configuration will be limited initially as a feature will be developed over time. It's important to consider what may be added in future.

Examples of things that typically require extending later:
* Connection info to other systems.
* An action that retrieves information from a domain object e.g. `request.body`, `request.header`

Often adding container objects can help.

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


### General naming guidelines
1. Use snake case for field names and enum variants.
2. Use terminology that is consistent with the feature that you are implementing. e.g. telemetry should use otel terminology.
3. Split terms with underscore. e.g. `export_url` not `exporturl`. Note that even though `url` is an acronym it is still split from `export`.
