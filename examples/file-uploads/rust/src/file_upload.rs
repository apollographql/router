use std::ops::ControlFlow;

use apollo_router::layers::ServiceBuilderExt;
use apollo_router::plugin::Plugin;
use apollo_router::plugin::PluginInit;
use apollo_router::register_plugin;
use apollo_router::services::router;
use apollo_router::services::subgraph;
use apollo_router::services::supergraph;
use hyper::header::CONTENT_TYPE;
use hyper::Body;
use multer::Constraints;
use multer::Multipart;
use multer::SizeLimit;
use reqwest::blocking::multipart;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

#[derive(Debug)]
struct FileUpload {
    #[allow(dead_code)]
    configuration: Conf,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
struct Conf {
    max_file_size: u64,  // The max file size in MB
    max_file_count: u64, // The max number of files in a single request
}

#[async_trait::async_trait]
impl Plugin for FileUpload {
    type Config = Conf;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        Ok(FileUpload {
            configuration: init.config,
        })
    }

    fn router_service(&self, service: router::BoxService) -> router::BoxService {
        let request_handler = move |mut req: router::Request| async {
            println!("req: {:#?}", req.router_request);

            // get content-type from request
            let content_type = req.router_request.headers().get("content-type").unwrap();
            // check if content-type contians "multipart/form-data"
            let is_multipart = content_type
                .to_str()
                .unwrap()
                .contains("multipart/form-data");

            println!(
                "content-type: {:?}, is_multipart: {}",
                content_type, is_multipart
            );

            // only process for multipart
            if is_multipart {
                // add is_multipart to the request context
                req.context.insert("is_multipart", is_multipart)?;

                // TODO: create list of approved fields, including range 0-max_file_count
                // let approved_fields = vec!["operations", "map"];

                // create constraints for the multipart stream
                let constraints = Constraints::new()
                    // .allowed_fields(approved_fields)
                    .size_limit(
                        SizeLimit::new()
                            // Set 10mb as size limit for all fields.
                            // TODO: get this from the config as max_file_size
                            .per_field(10 * 1024 * 1024),
                    );

                // get the request boundary
                let boundary = req
                    .router_request
                    .headers()
                    .get(CONTENT_TYPE)
                    .and_then(|ct| ct.to_str().ok())
                    .and_then(|ct| multer::parse_boundary(ct).ok());

                // process the incoming request
                {
                    // get the request body
                    let request_body = req.router_request.body_mut();

                    // create a multipart instance from request body and the constraints.
                    let mut multipart =
                        Multipart::with_constraints(request_body, boundary.unwrap(), constraints);

                    // create a new multipart form to store the fields in the request context
                    // TODO: create something to store the files in the context
                    // req.context.insert("multipart_form_data", multipart::Form::new())?;

                    // itterate over the multipart fields
                    while let Some(mut field) = multipart.next_field().await? {
                        // log field to console
                        println!("field: {:#?}, file: {:?}", field.name(), field.file_name());

                        let field_name = field.name();

                        // match based on field name, these names follow the graphql multipart spec
                        match field_name {
                            // if field name is "operations"
                            Some("operations") => {
                                // add the operations to the multipart context
                                let operations = field.text().await.unwrap();
                                println!("operations: {}", operations);
                                req.context.insert("multipart_operations", operations)?;
                            }
                            // if field name is "map"
                            Some("map") => {
                                // add the map to the multipart context
                                let map = field.text().await.unwrap();
                                println!("map: {}", map);
                                req.context.insert("multipart_map", map)?;
                            }
                            // if field name is anything else
                            unknown_field => {
                                // if field is a file add it to the requst context
                                if let Some(file_name) = &mut field.file_name() {
                                    println!(
                                        "field_name: {:?}, file_name: {:?}",
                                        unknown_field, file_name
                                    );

                                    // TODO: collect the chunks or bytes of the file
                                    while let Some(field_chunk) = field.chunk().await? {
                                        // Do something with field chunk.
                                        println!("field_chunk: {:?}", field_chunk);
                                    }

                                    // TODO: store the file in the request context
                                    // req.context.upsert("multipart_form_data", |form_data| form_data)?;
                                } else {
                                    // if it's just a text field and we have reached this point
                                    // then it's not a valid field, so throw an error?
                                    // Err(ControlFlow::Break("Invalid field!"));
                                }
                            }
                        }
                    }
                }

                let operations: Option<String> = req.context.get("multipart_operations")?;

                // create a new request with the new body
                Ok(ControlFlow::Continue(router::Request::from(
                    req.router_request.map(|_| Body::from(operations.unwrap())),
                )))
            } else {
                // not a multipart file upload so we continue
                Ok(ControlFlow::Continue(req))
            }
        };

        ServiceBuilder::new()
            .oneshot_checkpoint_async(request_handler)
            // .map_response()
            // .rate_limit()
            // .checkpoint()
            // .timeout()
            .service(service)
            .boxed()
    }

    fn supergraph_service(&self, service: supergraph::BoxService) -> supergraph::BoxService {
        let request_handler = move |mut req: supergraph::Request| {
            // the multipart spec replaces the upload variables with Null
            // if they are non-nullable, the router will complain that the upload variables are invalid
            // so we need to add them back in to make our request valid again

            // check if the request is multipart
            // TODO: why does get not work here?
            // let is_multipart = req.context.contains_key("is_multipart");
            let is_multipart = true;

            // if the request is multipart
            if is_multipart {
                // get the multipart map from the request context
                // let map: Option<String> = req.context.get("multipart_map")?;

                // get the variables from the request
                let variables = req.supergraph_request.body().variables.clone();

                // replace the variables using the multipart map
                // maybe using the file name as a placeholder
                // TODO: replace the variables with the multipart map

                // create a new request with the new body
                supergraph::Request::from(
                    req.supergraph_request, // .map(|_| Body::from(operations.unwrap())),
                )
            } else {
                // not a multipart file upload so we continue
                req
            }
        };

        ServiceBuilder::new()
            .map_request(request_handler)
            // .map_response()
            // .rate_limit()
            // .checkpoint()
            // .timeout()
            .service(service)
            .boxed()
    }

    // Called for each subgraph
    fn subgraph_service(&self, _name: &str, service: subgraph::BoxService) -> subgraph::BoxService {
        let request_handler = move |mut req: subgraph::Request| {
            // check if the request is multipart
            // TODO: why does get not work here?
            // let is_multipart = req.context.contains_key("is_multipart");
            let is_multipart = true;

            // check if this subgraph has a file upload
            // the best way to do this is probably compare the variables to the multipart map
            // TODO: check if this subgraph has a file upload
            let is_multipart_subgraph = true;

            // if the request is multipart
            if is_multipart && is_multipart_subgraph {
                // create a multipart form, this will be used to store the fields for subgraph requests
                let form_data = multipart::Form::new();

                // get the operations from the subgraph request
                // let subgraph_operation = req.subgraph_request.body();

                // add the operations to the new multipart form
                // form_data.text("operations", subgraph_operation);

                // get the multipart map from the request context
                // let multipart_map: Option<String> = req.context.get("multipart_map")?;

                // add the map to the new multipart form
                // form_data.text("map", multipart_map);

                // get the files from the request context
                // let multipart_form_data: Option<multipart::Form> = req.context.get("multipart_form_data")?;

                // construct a custom multipart part for each file
                // let filepart = multipart::Part::bytes(multipart_form_data);

                // add the files to the new multipart form
                // form_data.part(name, filepart)

                // create a new multipart request with the form data

                req
            } else {
                // not a multipart file upload so we continue
                req
            }
        };

        ServiceBuilder::new()
            .map_request(request_handler)
            // .map_response()
            // .rate_limit()
            // .checkpoint()
            // .timeout()
            .service(service)
            .boxed()
    }
}

// This macro allows us to use it in our plugin registry!
register_plugin!("example", "file_upload", FileUpload);

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn display_message() {
        let config = serde_json::json!({
            "plugins": {
                "example.file_upload": {
                    "name": "Bob"
                }
            }
        });
        // Build a test harness. Usually we'd use this and send requests to
        // it, but in this case it's enough to build the harness to see our
        // output when our service registers.
        let _test_harness = apollo_router::TestHarness::builder()
            .configuration_json(config)
            .unwrap()
            .build_router()
            .await
            .unwrap();
    }
}
