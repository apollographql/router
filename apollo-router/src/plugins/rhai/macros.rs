//! Various macros used in our rhai module and testing.

macro_rules! gen_map_request {
    ($base: ident, $borrow: ident, $rhai_service: ident, $callback: ident) => {
        $borrow.replace(|service| {
            fn rhai_service_span() -> impl Fn(&$base::Request) -> tracing::Span + Clone {
                move |_request: &$base::Request| {
                    tracing::info_span!(
                        "rhai plugin",
                        "rhai service" = stringify!($base::Request),
                        "otel.kind" = %SpanKind::Internal
                    )
                }
            }
            ServiceBuilder::new()
                .instrument(rhai_service_span())
                .checkpoint(move |request: $base::Request| {
                    // Let's define a local function to build an error response
                    fn failure_message(
                        context: Context,
                        error_details: ErrorDetails,
                    ) -> Result<ControlFlow<$base::Response, $base::Request>, BoxError>
                    {
                        let res = $base::Response::error_builder()
                            .errors(vec![Error {
                                message: error_details.message,
                                ..Default::default()
                            }])
                            .status_code(error_details.status)
                            .context(context)
                            .build()?;
                        Ok(ControlFlow::Break(res))
                    }
                    let shared_request = Shared::new(Mutex::new(Some(request)));
                    let result: Result<Dynamic, Box<EvalAltResult>> = if $callback.is_curried() {
                        $callback
                            .call(
                                &$rhai_service.engine,
                                &$rhai_service.ast,
                                (shared_request.clone(),),
                            )
                    } else {
                        let mut guard = $rhai_service.scope.lock().unwrap();
                        $rhai_service
                            .engine
                            .call_fn(
                                &mut guard,
                                &$rhai_service.ast,
                                $callback.fn_name(),
                                (shared_request.clone(),),
                            )
                    };
                    if let Err(error) = result {
                        let error_details = process_error(error);
                        tracing::error!("map_request callback failed: {error_details}");
                        let mut guard = shared_request.lock().unwrap();
                        let request_opt = guard.take();
                        return failure_message(
                            request_opt.unwrap().context,
                            error_details,
                        );
                    }
                    let mut guard = shared_request.lock().unwrap();
                    let request_opt = guard.take();
                    Ok(ControlFlow::Continue(request_opt.unwrap()))
                })
                .service(service)
                .boxed()
        })
    };
}

// Actually use the checkpoint function so that we can shortcut requests which fail
macro_rules! gen_map_deferred_request {
    ($request: ident, $response: ident, $borrow: ident, $rhai_service: ident, $callback: ident) => {
        $borrow.replace(|service| {
            fn rhai_service_span() -> impl Fn(&$request) -> tracing::Span + Clone {
                move |_request: &$request| {
                    tracing::info_span!(
                        "rhai plugin",
                        "rhai service" = stringify!($request),
                        "otel.kind" = %SpanKind::Internal
                    )
                }
            }
            ServiceBuilder::new()
                .instrument(rhai_service_span())
                .checkpoint(move |request: $request| {
                    // Let's define a local function to build an error response
                    fn failure_message(
                        context: Context,
                        error_details: ErrorDetails,
                    ) -> Result<ControlFlow<$response, $request>, BoxError> {
                        let res = $response::error_builder()
                            .errors(vec![Error {
                                message: error_details.message,
                                ..Default::default()
                            }])
                            .status_code(error_details.status)
                            .context(context)
                            .build()?;
                        Ok(ControlFlow::Break(res))
                    }
                    let shared_request = Shared::new(Mutex::new(Some(request)));
                    let result = execute(&$rhai_service, &$callback, (shared_request.clone(),));

                    if let Err(error) = result {
                        tracing::error!("map_request callback failed: {error}");
                        let error_details = process_error(error);
                        let mut guard = shared_request.lock().unwrap();
                        let request_opt = guard.take();
                        return failure_message(
                            request_opt.unwrap().context,
                            error_details
                        );
                    }
                    let mut guard = shared_request.lock().unwrap();
                    let request_opt = guard.take();
                    Ok(ControlFlow::Continue(request_opt.unwrap()))
                })
                .service(service)
                .boxed()
        })
    };
}

// Actually use the checkpoint_async function so that we can shortcut requests which fail
macro_rules! gen_map_async_request {
    ($base: ident, $borrow: ident, $rhai_service: ident, $callback: ident, $url: ident, $step: ident) => {
        // Clone all our arguments (or wrap in Arc)
        let rs = Arc::new($rhai_service);
        let cb = Arc::new($callback);
        let u = $url.clone();
        $borrow.replace(move |service| {
            fn rhai_service_span() -> impl Fn(&$base::Request) -> tracing::Span + Clone {
                move |_request: &$base::Request| {
                    tracing::info_span!(
                        "rhai plugin",
                        "rhai service" = stringify!($base::Request),
                        "otel.kind" = %SpanKind::Internal
                    )
                }
            }
            ServiceBuilder::new()
                .instrument(rhai_service_span())
                .checkpoint_async(move |mut request: $base::Request| {
                    let mut check_res = None;
                    // Let's define a local function to build an error response
                    fn failure_message(
                        context: Context,
                        error_details: ErrorDetails,
                    ) -> Result<ControlFlow<$base::Response, $base::Request>, BoxError>
                    {
                        let res = $base::Response::error_builder()
                            .errors(vec![Error {
                                message: error_details.message,
                                ..Default::default()
                            }])
                            .status_code(error_details.status)
                            .context(context)
                            .build()?;
                        Ok(ControlFlow::Break(res))
                    }
                    /*
                    let shared_request = Shared::new(Mutex::new(Some(request)));
                    let result: Result<Dynamic, Box<EvalAltResult>> = if $callback.is_curried() {
                        $callback
                            .call(
                                &$rhai_service.engine,
                                &$rhai_service.ast,
                                (shared_request.clone(),),
                            )
                    } else {
                        let mut guard = $rhai_service.scope.lock().unwrap();
                        $rhai_service
                            .engine
                            .call_fn(
                                &mut guard,
                                &$rhai_service.ast,
                                $callback.fn_name(),
                                (shared_request.clone(),),
                            )
                    };
                    if let Err(error) = result {
                        let error_details = process_error(error);
                        tracing::error!("map_request callback failed: {error_details}");
                        let mut guard = shared_request.lock().unwrap();
                        let request_opt = guard.take();
                        check_res = Some(failure_message(
                            request_opt.unwrap().context,
                            error_details,
                        ));
                    }
                    let mut guard = shared_request.lock().unwrap();
                    let request_opt = guard.take();
                    async {
                    match check_res {
                        Some(res) => res,
                        None => Ok(ControlFlow::Continue(request_opt.unwrap()))
                    }
                    }
                    */
                    // Clone all our arguments again...
                    let rs_rs = rs.clone();
                    let cb_cb = cb.clone();
                    let u_u = u.clone();
                    async move {
                        // Need to do a dance to go and get some data before we callback to rhai
                        let body = request.supergraph_request.body().clone();
                        let context = request.context.clone();
                        let sdl = SDL
                            .read()
                            .expect("acquiring SDL read lock")
                            .to_string();

                        let headers = request.supergraph_request.headers();

                        let output = call_external(u_u.clone(), PipelineStep::$step, headers, body, context, sdl).await?;

                        // Cannot update body or headers for subgraph request
                        // *request.supergraph_request.headers_mut() = internalize_header_map(output.headers)?;
                        // *request.supergraph_request.body_mut() = output.body;
                        request.context = output.context;
                        let shared_request = Shared::new(Mutex::new(Some(request)));
                        tracing::info!("about to callback to rhai");
                        let result = execute(&rs_rs.clone(), &cb_cb.clone(), (shared_request.clone(),));
                        tracing::info!("just after callback to rhai");

                        if let Err(error) = result {
                            tracing::error!("map_request callback failed: {error}");
                            let error_details = process_error(error);
                            let mut guard = shared_request.lock().unwrap();
                            let request_opt = guard.take();
                            check_res = Some(failure_message(
                                request_opt.unwrap().context,
                                error_details
                            ));
                        }
                        let mut guard = shared_request.lock().unwrap();
                        let request_opt = guard.take();
                        match check_res {
                            Some(res) => res,
                            None => Ok(ControlFlow::Continue(request_opt.unwrap()))
                        }
                    }
                })
                .buffer(20_000)
                .service(service)
                .boxed()
        })
    };
}

// Actually use the checkpoint_async function so that we can shortcut requests which fail
macro_rules! gen_map_deferred_async_request {
    ($request: ident, $response: ident, $borrow: ident, $rhai_service: ident, $callback: ident, $url: ident, $step: ident) => {
        // Clone all our arguments (or wrap in Arc)
        let rs = Arc::new($rhai_service);
        let cb = Arc::new($callback);
        let u = $url.clone();
        $borrow.replace(|service| {
            fn rhai_service_span() -> impl Fn(&$request) -> tracing::Span + Clone {
                move |_request: &$request| {
                    tracing::info_span!(
                        "rhai plugin",
                        "rhai service" = stringify!($request),
                        "otel.kind" = %SpanKind::Internal
                    )
                }
            }
            ServiceBuilder::new()
                .instrument(rhai_service_span())
                .checkpoint_async(move |mut request: $request| {
                    let mut check_res = None;
                    // Let's define a local function to build an error response
                    fn failure_message(
                        context: Context,
                        error_details: ErrorDetails,
                    ) -> Result<ControlFlow<$response, $request>, BoxError> {
                        let res = $response::error_builder()
                            .errors(vec![Error {
                                message: error_details.message,
                                ..Default::default()
                            }])
                            .status_code(error_details.status)
                            .context(context)
                            .build()?;
                        Ok(ControlFlow::Break(res))
                    }
                    // Clone all our arguments again...
                    let rs_rs = rs.clone();
                    let cb_cb = cb.clone();
                    let u_u = u.clone();
                    async move {
                        // Need to do a dance to go and get some data before we callback to rhai
                        let body = request.supergraph_request.body().clone();
                        let context = request.context.clone();
                        let sdl = SDL
                            .read()
                            .expect("acquiring SDL read lock")
                            .to_string();

                        let headers = request.supergraph_request.headers();

                        let output = call_external(u_u.clone(), PipelineStep::$step, headers, body, context, sdl).await?;

                        *request.supergraph_request.headers_mut() = internalize_header_map(output.headers)?;
                        *request.supergraph_request.body_mut() = output.body;
                        request.context = output.context;
                        let shared_request = Shared::new(Mutex::new(Some(request)));
                        tracing::info!("about to callback to rhai");
                        let result = execute(&rs_rs.clone(), &cb_cb.clone(), (shared_request.clone(),));
                        tracing::info!("just after callback to rhai");

                        if let Err(error) = result {
                            tracing::error!("map_request callback failed: {error}");
                            let error_details = process_error(error);
                            let mut guard = shared_request.lock().unwrap();
                            let request_opt = guard.take();
                            check_res = Some(failure_message(
                                request_opt.unwrap().context,
                                error_details
                            ));
                        }
                        let mut guard = shared_request.lock().unwrap();
                        let request_opt = guard.take();
                        match check_res {
                            Some(res) => res,
                            None => Ok(ControlFlow::Continue(request_opt.unwrap()))
                        }
                    }
                })
                .buffer(20_000)
                .service(service)
                .boxed()
        })
    };
}

macro_rules! gen_map_response {
    ($base: ident, $borrow: ident, $rhai_service: ident, $callback: ident) => {
        $borrow.replace(|service| {
            service
                .map_response(move |response: $base::Response| {
                    // Let's define a local function to build an error response
                    // XXX: This isn't ideal. We already have a response, so ideally we'd
                    // like to append this error into the existing response. However,
                    // the significantly different treatment of errors in different
                    // response types makes this extremely painful. This needs to be
                    // re-visited at some point post GA.
                    fn failure_message(
                        context: Context,
                        error_details: ErrorDetails,
                    ) -> $base::Response {
                        let res = $base::Response::error_builder()
                            .errors(vec![Error {
                                message: error_details.message,
                                ..Default::default()
                            }])
                            .status_code(error_details.status)
                            .context(context)
                            .build()
                            .expect("can't fail to build our error message");
                        res
                    }
                    let shared_response = Shared::new(Mutex::new(Some(response)));
                    let result: Result<Dynamic, Box<EvalAltResult>> = if $callback.is_curried() {
                        $callback.call(
                            &$rhai_service.engine,
                            &$rhai_service.ast,
                            (shared_response.clone(),),
                        )
                    } else {
                        let mut guard = $rhai_service.scope.lock().unwrap();
                        $rhai_service.engine.call_fn(
                            &mut guard,
                            &$rhai_service.ast,
                            $callback.fn_name(),
                            (shared_response.clone(),),
                        )
                    };
                    if let Err(error) = result {
                        tracing::error!("map_response callback failed: {error}");
                        let error_details = process_error(error);
                        let mut guard = shared_response.lock().unwrap();
                        let response_opt = guard.take();
                        return failure_message(response_opt.unwrap().context, error_details);
                    }
                    let mut guard = shared_response.lock().unwrap();
                    let response_opt = guard.take();
                    response_opt.unwrap()
                })
                .boxed()
        })
    };
}

macro_rules! gen_map_deferred_response {
    ($response: ident, $rhai_response: ident, $rhai_deferred_response: ident, $borrow: ident, $rhai_service: ident, $callback: ident) => {
        $borrow.replace(|service| {
            BoxService::new(service.and_then(
                |mapped_response: $response| async move {
                    // Let's define a local function to build an error response
                    // XXX: This isn't ideal. We already have a response, so ideally we'd
                    // like to append this error into the existing response. However,
                    // the significantly different treatment of errors in different
                    // response types makes this extremely painful. This needs to be
                    // re-visited at some point post GA.
                    fn failure_message(
                        context: Context,
                        error_details: ErrorDetails,
                    ) -> $response {
                        let res = $response::error_builder()
                            .errors(vec![Error {
                                message: error_details.message,
                                ..Default::default()
                            }])
                            .status_code(error_details.status)
                            .context(context)
                            .build()
                            .expect("can't fail to build our error message");
                        res
                    }

                    // we split the response stream into headers+first response, then a stream of deferred responses
                    // for which we will implement mapping later
                    let $response { response, context } = mapped_response;
                    let (parts, stream) = response.into_parts();
                    let (first, rest) = stream.into_future().await;

                    if first.is_none() {
                        let error_details = ErrorDetails {
                            status: StatusCode::INTERNAL_SERVER_ERROR,
                            message: "rhai execution error: empty response".to_string(),
                            position: None
                        };
                        return Ok(failure_message(
                            context,
                            error_details
                        ));
                    }

                    let response = $rhai_response {
                        context,
                        response: http::Response::from_parts(
                            parts,
                            first.expect("already checked"),
                        )
                        .into(),
                    };
                    let shared_response = Shared::new(Mutex::new(Some(response)));

                    let result =
                        execute(&$rhai_service, &$callback, (shared_response.clone(),));
                    if let Err(error) = result {
                        tracing::error!("map_response callback failed: {error}");
                        let error_details = process_error(error);
                        let mut guard = shared_response.lock().unwrap();
                        let response_opt = guard.take();
                        return Ok(failure_message(
                            response_opt.unwrap().context,
                            error_details
                        ));
                    }

                    let mut guard = shared_response.lock().unwrap();
                    let response_opt = guard.take();
                    let $rhai_response { context, response } =
                        response_opt.unwrap();
                    let (parts, body) = http::Response::from(response).into_parts();

                    let ctx = context.clone();

                    let mapped_stream = rest.filter_map(move |deferred_response| {
                        let rhai_service = $rhai_service.clone();
                        let context = context.clone();
                        let callback = $callback.clone();
                        async move {
                            let response = $rhai_deferred_response {
                                context,
                                response: deferred_response,
                            };
                            let shared_response = Shared::new(Mutex::new(Some(response)));

                            let result = execute(
                                &rhai_service,
                                &callback,
                                (shared_response.clone(),),
                            );
                            if let Err(error) = result {
                                tracing::error!("map_response callback failed: {error}");
                                return None;
                            }

                            let mut guard = shared_response.lock().unwrap();
                            let response_opt = guard.take();
                            let $rhai_deferred_response { response, .. } =
                                response_opt.unwrap();
                            Some(response)
                        }
                    });

                    let response = http::Response::from_parts(
                        parts,
                        once(ready(body)).chain(mapped_stream).boxed(),
                    )
                    .into();
                    Ok($response {
                        context: ctx,
                        response,
                    })
                },
            ))
        })
    };
}

macro_rules! if_subgraph {
    ( subgraph => $subgraph: block else $not_subgraph: block ) => {
        $subgraph
    };
    ( $base: ident => $subgraph: block else $not_subgraph: block ) => {
        $not_subgraph
    };
}

macro_rules! register_rhai_interface {
    ($engine: ident, $($base: ident), *) => {
        $(
            // Context stuff
            $engine.register_get(
                "context",
                |obj: &mut SharedMut<$base::Request>| -> Result<Context, Box<EvalAltResult>> {
                    Ok(obj.with_mut(|request| request.context.clone()))
                }
            )
            .register_get(
                "context",
                |obj: &mut SharedMut<$base::Response>| -> Result<Context, Box<EvalAltResult>> {
                    Ok(obj.with_mut(|response| response.context.clone()))
                }
            );

            $engine.register_set(
                "context",
                |obj: &mut SharedMut<$base::Request>, context: Context| {
                    obj.with_mut(|request| request.context = context);
                    Ok(())
                }
            )
            .register_set(
                "context",
                |obj: &mut SharedMut<$base::Response>, context: Context| {
                    obj.with_mut(|response| response.context = context);
                    Ok(())
                }
            );

            // Originating Request
            $engine.register_get(
                "headers",
                |obj: &mut SharedMut<$base::Request>| -> Result<HeaderMap, Box<EvalAltResult>> {
                    Ok(obj.with_mut(|request| request.supergraph_request.headers().clone()))
                }
            );

            $engine.register_set(
                "headers",
                |obj: &mut SharedMut<$base::Request>, headers: HeaderMap| {
                    macros::if_subgraph! {
                        $base => {
                            let _unused = (obj, headers);
                            Err("cannot mutate originating request on a subgraph".into())
                        } else {
                            obj.with_mut(|request| *request.supergraph_request.headers_mut() = headers);
                            Ok(())
                        }
                    }
                }
            );

            $engine.register_get(
                "body",
                |obj: &mut SharedMut<$base::Request>| -> Result<Request, Box<EvalAltResult>> {
                    Ok(obj.with_mut(|request| request.supergraph_request.body().clone()))
                }
            );

            $engine.register_set(
                "body",
                |obj: &mut SharedMut<$base::Request>, body: Request| {
                    macros::if_subgraph! {
                        $base => {
                            let _unused = (obj, body);
                            Err("cannot mutate originating request on a subgraph".into())
                        } else {
                            obj.with_mut(|request| *request.supergraph_request.body_mut() = body);
                            Ok(())
                        }
                    }
                }
            );

            $engine.register_get(
                "uri",
                |obj: &mut SharedMut<$base::Request>| -> Result<Uri, Box<EvalAltResult>> {
                    Ok(obj.with_mut(|request| request.supergraph_request.uri().clone()))
                }
            );

            $engine.register_set(
                "uri",
                |obj: &mut SharedMut<$base::Request>, uri: Uri| {
                    macros::if_subgraph! {
                        $base => {
                            let _unused = (obj, uri);
                            Err("cannot mutate originating request on a subgraph".into())
                        } else {
                            obj.with_mut(|request| *request.supergraph_request.uri_mut() = uri);
                            Ok(())
                        }
                    }
                }
            );
        )*
    };
}

// There is a lot of repetition in these tests, so I've tried to reduce that with these two
// macros. The repetition could probably be reduced further, but ...

#[cfg(test)]
macro_rules! gen_request_test {
    ($base: ident, $fn_name: literal) => {
        let dyn_plugin: Box<dyn DynPlugin> = crate::plugin::plugins()
            .get("apollo.rhai")
            .expect("Plugin not found")
            .create_instance_without_schema(
                &Value::from_str(
                    r#"{"scripts":"tests/fixtures", "main":"request_response_test.rhai"}"#,
                )
                .unwrap(),
            )
            .await
            .unwrap();

        // Downcast our generic plugin. We know it must be Rhai
        let it: &dyn std::any::Any = dyn_plugin.as_any();
        let rhai_instance: &Rhai = it.downcast_ref::<Rhai>().expect("downcast");

        // Get a scope to use for our test
        let scope = rhai_instance.scope.clone();

        let mut guard = scope.lock().unwrap();

        // We must wrap our canned request in Arc<Mutex<Option<>>> to keep the rhai runtime
        // happy
        let request = Arc::new(Mutex::new(Some($base::fake_builder().build())));

        // Call our rhai test function. If it return an error, the test failed.
        let result: Result<(), Box<rhai::EvalAltResult>> =
            rhai_instance
                .engine
                .call_fn(&mut guard, &rhai_instance.ast, $fn_name, (request,));
        result.expect("test failed");
    };
}

#[cfg(test)]
macro_rules! gen_response_test {
    ($base: ident, $fn_name: literal) => {
        let dyn_plugin: Box<dyn DynPlugin> = crate::plugin::plugins()
            .get("apollo.rhai")
            .expect("Plugin not found")
            .create_instance_without_schema(
                &Value::from_str(
                    r#"{"scripts":"tests/fixtures", "main":"request_response_test.rhai"}"#,
                )
                .unwrap(),
            )
            .await
            .unwrap();

        // Downcast our generic plugin. We know it must be Rhai
        let it: &dyn std::any::Any = dyn_plugin.as_any();
        let rhai_instance: &Rhai = it.downcast_ref::<Rhai>().expect("downcast");

        // Get a scope to use for our test
        let scope = rhai_instance.scope.clone();

        let mut guard = scope.lock().unwrap();

        // We must wrap our canned response in Arc<Mutex<Option<>>> to keep the rhai runtime
        // happy
        let response = Arc::new(Mutex::new(Some($base::default())));

        // Call our rhai test function. If it return an error, the test failed.
        let result: Result<(), Box<rhai::EvalAltResult>> =
            rhai_instance
                .engine
                .call_fn(&mut guard, &rhai_instance.ast, $fn_name, (response,));
        result.expect("test failed");
    };
}

pub(super) use gen_map_async_request;
pub(super) use gen_map_deferred_async_request;
pub(super) use gen_map_deferred_request;
pub(super) use gen_map_deferred_response;
pub(super) use gen_map_request;
pub(super) use gen_map_response;
#[cfg(test)]
pub(super) use gen_request_test;
#[cfg(test)]
pub(super) use gen_response_test;
pub(super) use if_subgraph;
pub(super) use register_rhai_interface;
