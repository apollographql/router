use apollo_compiler::name;
use apollo_compiler::schema::Name;

pub(crate) const SOURCE_DIRECTIVE_NAME_IN_SPEC: Name = name!("source");
pub(crate) const SOURCE_NAME_ARGUMENT_NAME: Name = name!("name");
pub(crate) const SOURCE_HTTP_ARGUMENT_NAME: Name = name!("http");
pub(crate) const SOURCE_BASE_URL_ARGUMENT_NAME: Name = name!("baseURL");

pub(crate) const CONNECT_DIRECTIVE_NAME_IN_SPEC: Name = name!("connect");
pub(crate) const CONNECT_SOURCE_ARGUMENT_NAME: Name = name!("source");
pub(crate) const CONNECT_HTTP_ARGUMENT_NAME: Name = name!("http");
pub(crate) const CONNECT_SELECTION_ARGUMENT_NAME: Name = name!("selection");
