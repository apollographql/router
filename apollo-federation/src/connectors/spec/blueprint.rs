use crate::connectors::ConnectSpec;
use crate::error::FederationError;
use crate::schema::FederationSchema;

pub(crate) struct ConnectBlueprint {}

impl ConnectBlueprint {
    pub(crate) fn on_directive_definition_and_schema_parsed(
        schema: &mut FederationSchema,
    ) -> Result<(), FederationError> {
        ConnectSpec::check_or_add(schema)?;
        Ok(())
    }

    pub(crate) fn on_constructed(_schema: &mut FederationSchema) -> Result<(), FederationError> {
        Ok(())
    }
}
