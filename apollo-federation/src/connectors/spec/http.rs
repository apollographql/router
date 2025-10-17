use apollo_compiler::Name;
use apollo_compiler::name;

pub(crate) const HTTP_HEADER_MAPPING_NAME_IN_SPEC: Name = name!("HTTPHeaderMapping");
pub(crate) const HTTP_HEADER_MAPPING_NAME_ARGUMENT_NAME: Name = name!("name");
pub(crate) const HTTP_HEADER_MAPPING_FROM_ARGUMENT_NAME: Name = name!("from");
pub(crate) const HTTP_HEADER_MAPPING_VALUE_ARGUMENT_NAME: Name = name!("value");
pub(crate) const HTTP_ARGUMENT_NAME: Name = name!("http");
pub(crate) const HEADERS_ARGUMENT_NAME: Name = name!("headers");

pub(crate) const PATH_ARGUMENT_NAME: Name = name!("path");
pub(crate) const QUERY_PARAMS_ARGUMENT_NAME: Name = name!("queryParams");

pub(crate) const URL_PATH_TEMPLATE_SCALAR_NAME: Name = name!("URLTemplate");
