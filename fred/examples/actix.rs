use actix_web::{
  body::BoxBody,
  http::StatusCode,
  middleware,
  web::{delete, get, post, resource, Data, Path},
  App,
  HttpResponse,
  HttpServer,
};
use bytes::Bytes;
use fred::prelude::*;
use log::{debug, info};
use serde::Deserialize;
use std::{env, io, str, time::Duration};

#[derive(Debug, Deserialize)]
struct KeyPath {
  key: String,
}

#[actix_web::main]
async fn main() -> io::Result<()> {
  pretty_env_logger::init();

  let pool_size = env::var("REDIS_POOL_SIZE")
    .ok()
    .and_then(|v| v.parse::<usize>().ok())
    .unwrap_or(8);
  let config = Config::from_url("redis://foo:bar@127.0.0.1:6379").unwrap();
  let pool = Builder::from_config(config)
    .with_connection_config(|config| {
      config.connection_timeout = Duration::from_secs(10);
    })
    // use exponential backoff, starting at 100 ms and doubling on each failed attempt up to 30 sec
    .set_policy(ReconnectPolicy::new_exponential(0, 100, 30_000, 2))
    .build_pool(pool_size)
    .expect("Failed to create redis pool");

  pool.init().await.expect("Failed to connect to redis");
  info!("Connected to Redis");

  HttpServer::new(move || {
    App::new()
      .app_data(Data::new(pool.clone()))
      .service(
        resource("/{key}")
          .route(get().to(get_key))
          .route(post().to(set_key))
          .route(delete().to(del_key)),
      )
      .service(resource("/{key}/incr").route(post().to(incr_key)))
      .wrap(middleware::NormalizePath::trim())
  })
  .workers(2)
  .bind(("127.0.0.1", 3000))?
  .run()
  .await
}

fn map_error(err: Error) -> (StatusCode, Bytes) {
  let details = err.details().to_string().into();
  let code = if *err.kind() == ErrorKind::NotFound {
    StatusCode::NOT_FOUND
  } else if err.details().starts_with("WRONGTYPE") {
    StatusCode::BAD_REQUEST
  } else {
    StatusCode::INTERNAL_SERVER_ERROR
  };

  (code, details)
}

fn map_response(code: StatusCode, body: Bytes) -> HttpResponse {
  HttpResponse::new(code).set_body(BoxBody::new(body))
}

async fn get_key(pool: Data<Pool>, params: Path<KeyPath>) -> HttpResponse {
  debug!("get {}", params.key);

  let (code, val) = match pool.get::<Option<Bytes>, _>(&params.key).await {
    Ok(Some(val)) => (StatusCode::OK, val),
    Ok(None) => (StatusCode::NOT_FOUND, "Not found".into()),
    Err(err) => map_error(err),
  };

  map_response(code, val)
}

async fn set_key(pool: Data<Pool>, params: Path<KeyPath>, body: Bytes) -> HttpResponse {
  debug!("set {} {}", params.key, String::from_utf8_lossy(&body));

  let (code, val) = match pool.set::<Bytes, _, _>(&params.key, body, None, None, false).await {
    Ok(val) => (StatusCode::OK, val),
    Err(err) => map_error(err),
  };

  map_response(code, val)
}

async fn del_key(pool: Data<Pool>, params: Path<KeyPath>) -> HttpResponse {
  debug!("del {}", params.key);

  let (code, val) = match pool.del::<i64, _>(&params.key).await {
    Ok(0) => (StatusCode::NOT_FOUND, "Not Found.".into()),
    Ok(val) => (StatusCode::OK, val.to_string().into()),
    Err(err) => map_error(err),
  };

  map_response(code, val)
}

async fn incr_key(pool: Data<Pool>, params: Path<KeyPath>, body: Bytes) -> HttpResponse {
  let count = str::from_utf8(&body)
    .ok()
    .and_then(|s| s.parse::<i64>().ok())
    .unwrap_or(1);
  debug!("incr {} by {}", params.key, count);

  let (code, val) = match pool.incr_by::<i64, _>(&params.key, count).await {
    Ok(val) => (StatusCode::OK, val.to_string().into()),
    Err(err) => map_error(err),
  };

  map_response(code, val)
}

// example usage with curl:
// $ curl http://localhost:3000/foo
// Not found
// $ curl -X POST -d '100' http://localhost:3000/foo
// OK
// $ curl -X POST -d '50' http://localhost:3000/foo/incr
// 150
// $ curl -X POST -d '50' http://localhost:3000/foo/incr
// 200
// $ curl -X POST -d '50' http://localhost:3000/foo/incr
// 250
// $ curl http://localhost:3000/foo
// 250
// $ curl -X DELETE http://localhost:3000/foo
// 1
// $ curl http://localhost:3000/foo
// Not found
