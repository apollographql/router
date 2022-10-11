# Changelog for the next release

All notable changes to Router will be documented in this file.

This project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

<!-- <THIS IS AN EXAMPLE, DO NOT REMOVE>

# [x.x.x] (unreleased) - 2022-mm-dd
> Important: X breaking changes below, indicated by **❗ BREAKING ❗**
## ❗ BREAKING ❗
## 🚀 Features
## 🐛 Fixes
## 🛠 Maintenance
## 📚 Documentation

## Example section entry format

### Headline ([Issue #ISSUE_NUMBER](https://github.com/apollographql/router/issues/ISSUE_NUMBER))

Description! And a link to a [reference](http://url)

By [@USERNAME](https://github.com/USERNAME) in https://github.com/apollographql/router/pull/PULL_NUMBER
-->

# [x.x.x] (unreleased) - 2022-mm-dd

## ❗ BREAKING ❗

### Remove support for `rhai.input_file` ([Issue #1826](https://github.com/apollographql/router/issues/1826))

The existing `rhai.input_file` mechanism doesn't really work for most helm use cases. This PR removes this mechanism and and encourages the use of the `extraVolumes/extraVolumeMounts` mechanism with rhai.

Example: Create a configmap which contains your rhai scripts.

```yaml
apiVersion: v1
kind: ConfigMap
metadata:
  name: rhai-config
  labels:
    app.kubernetes.io/name: rhai-config
    app.kubernetes.io/instance: rhai-config
data:
  main.rhai: |
    // Call map_request with our service and pass in a string with the name
    // of the function to callback
    fn subgraph_service(service, subgraph) {
        print(`registering request callback for ${subgraph}`);
        const request_callback = Fn("process_request");
        service.map_request(request_callback);
    }
  
    // This will convert all cookie pairs into headers.
    // If you only wish to convert certain cookies, you
    // can add logic to modify the processing.
    fn process_request(request) {
  
        // Find our cookies
        if "cookie" in request.headers {
            print("adding cookies as headers");
            let cookies = request.headers["cookie"].split(';');
            for cookie in cookies {
                // Split our cookies into name and value
                let k_v = cookie.split('=', 2);
                if k_v.len() == 2 {
                    // trim off any whitespace
                    k_v[0].trim();
                    k_v[1].trim();
                    // update our headers
                    // Note: we must update subgraph.headers, since we are
                    // setting a header in our sub graph request
                    request.subgraph.headers[k_v[0]] = k_v[1];
                }
            }
        } else {
            print("no cookies in request");
        }
    }
  my-module.rhai: |
    fn process_request(request) {
        print("processing a request");
    }
```
Note how the data represents multiple rhai source files. The module code isn't used, it's just there to illustrate multiple files in a single configmap.

With that configmap in place, the helm chart can be used with a values file that contains:

```yaml
router:
  configuration:
    rhai:
      scripts: /dist/rhai
      main: main.rhai
extraVolumeMounts:
  - name: rhai-volume
    mountPath: /dist/rhai
    readonly: true
extraVolumes:
  - name: rhai-volume
    configMap:
      name: rhai-config
```
The configuration tells the router to load the rhai script `main.rhai` from the directory `/dist/rhai` (and load any imported modules from /dist/rhai)

This will mount the confimap created above in the `/dist/rhai` directory with two files:
 - `main.rhai`
 - `my-module.rhai`

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/1917

## 🚀 Features

### Expose the TraceId functionality to rhai ([Issue #1935](https://github.com/apollographql/router/issues/1935))
    
A new function, traceid(), is exposed to rhai scripts which may be used to retrieve a unique trace id for a request. The trace id is an opentelemetry span id.  

```
fn supergraph_service(service) {
    try {
        let id = traceid();
        print(`id: ${id}`);
    }
    catch(err)
    {
        // log any errors
        log_error(`span id error: ${err}`);
    }
}
```

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/1937

## 🐛 Fixes

### Fix studio reporting failures ([Issue #1903](https://github.com/apollographql/router/issues/1903))

The root cause of the issue was letting the server component of spaceport close silently during a re-configuration or schema reload. This fixes the issue by keeping the server component alive as long as the client remains connected.

Additionally, recycled spaceport connections are now re-connected to spaceport to further ensure connection validity.

Also make deadpool sizing constant across environments (#1893) 

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/1928

### Update `apollo-parser` to v0.2.12 ([PR #1921](https://github.com/apollographql/router/pull/1921))

Correctly lexes and creates an error token for unterminated GraphQL `StringValue`s with unicode and line terminator characters.

By [@lrlna](https://github.com/lrlna) in https://github.com/apollographql/router/pull/1921

### `traffic_shaping.all.deduplicate_query` was not correctly set ([PR #1901](https://github.com/apollographql/router/pull/1901))

Due to a change in our traffic_shaping configuration the `deduplicate_query` field for all subgraph wasn't set correctly.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1901

## 🛠 Maintenance

### Fix hpa yaml for appropriate kubernetes versions ([#1908](https://github.com/apollographql/router/pull/1908))

Correct schema for autoscaling/v2beta2 and autoscaling/v2 api versions of the
HorizontalPodAutoscaler within the helm chart

By [@damienpontifex](https://github.com/damienpontifex) in https://github.com/apollographql/router/issues/1914

## 📚 Documentation
