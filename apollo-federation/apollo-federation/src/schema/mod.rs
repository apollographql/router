use crate::link::LinksMetadata;
use apollo_compiler::Schema;
use referencer::Referencers;

pub(crate) mod position;
pub(crate) mod referencer;

pub(crate) struct FederationSchema {
    schema: Schema,
    metadata: Option<LinksMetadata>,
    referencers: Referencers,
}

impl FederationSchema {
    pub(crate) fn schema(&self) -> &Schema {
        &self.schema
    }

    pub(crate) fn metadata(&self) -> &Option<LinksMetadata> {
        &self.metadata
    }

    pub(crate) fn referencers(&self) -> &Referencers {
        &self.referencers
    }
}
