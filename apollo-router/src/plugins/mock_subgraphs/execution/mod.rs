//! APIs related to [executing a GraphQL request][execution]
//! and returning a [GraphQL response][response]
//!
//! [execution]: https://spec.graphql.org/October2021/#sec-Execution
//! [response]: https://spec.graphql.org/October2021/#sec-Response

#[macro_use]
pub(crate) mod resolver;
pub(crate) mod engine;
pub(crate) mod input_coercion;
pub(crate) mod result_coercion;
