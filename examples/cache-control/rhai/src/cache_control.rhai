fn subgraph_service(service, subgraph) {
  // collect the max-age and scope values from cache-control headers and store
  // on the context for use in supergraph_service
  service.map_response(|response| {
    let cache_control = response.headers.values("cache-control").get(0);

    // if a subgraph response is uncacheable, the whole response is uncacheable
    if cache_control == () {
      response.context.cache_control_uncacheable = true;
      return;
    }

    let max_age = get_max_age(cache_control);

    // use the smallest max age
    response.context.upsert("cache_control_max_age", |current| {
      if current == () {
        max_age
      } else if max_age < current {
        max_age
      } else {
        current
      }
    });

    let scope = if cache_control.contains("public") {
      "public"
    } else {
      "private"
    };

    // if the scope is ever private, it cannot become public
    response.context.upsert("cache_control_scope", |current| {
      if current == "private" || scope == "private" {
        "private"
      } else {
        scope
      }
    });
  });
}

fn supergraph_service(service) {
  // attach the cache-control header if enough data is available
  service.map_response(|response| {

    // Note the check for is_primary() to support deferred response headers
    // https://www.apollographql.com/docs/router/customizations/rhai-api/#response-interface
    if response.is_primary() {
      let uncacheable = response.context.cache_control_uncacheable;
      let max_age = response.context.cache_control_max_age;
      let scope = response.context.cache_control_scope;
  
      if uncacheable != true && max_age != () && scope != () {
        response.headers["cache-control"] = `max-age=${max_age}, ${scope}`;
      }
  }
  });
}

// find the the max-age= part and parse the value into an integer
fn get_max_age(str) {
  let max_age = 0;

  for part in str.split(",") {
    part.remove(" ");

    if part.starts_with("max-age=") {
      let num = part.split("=").get(1);

      if num == () || num == "" {
        break;
      }

      try {
        max_age = num.parse_int();
      } catch (err) {
        log_error(`error parsing max-age from "${str}": ${err}`);
      }
      break;
    }
  }

  max_age
}
