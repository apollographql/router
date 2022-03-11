use super::ExportConfig;
use crate::configuration::{ConfigurationError, TlsConfig};
use opentelemetry_otlp::WithExportConfig;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use tonic::metadata::{KeyAndValueRef, MetadataKey, MetadataMap};

#[derive(Debug, Clone, Deserialize, Serialize, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GrpcExporter {
    #[serde(flatten)]
    export_config: ExportConfig,
    tls_config: Option<TlsConfig>,
    #[serde(
        deserialize_with = "header_map_serde::deserialize",
        serialize_with = "header_map_serde::serialize",
        default
    )]
    #[schemars(schema_with = "option_metadata_map")]
    metadata: Option<MetadataMap>,
}

fn option_metadata_map(gen: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
    Option::<HashMap<String, Value>>::json_schema(gen)
}

impl GrpcExporter {
    pub fn exporter(&self) -> Result<opentelemetry_otlp::TonicExporterBuilder, ConfigurationError> {
        let mut exporter = opentelemetry_otlp::new_exporter().tonic();
        exporter = self.export_config.apply(exporter);
        #[allow(unused_variables)]
        if let Some(tls_config) = self.tls_config.as_ref() {
            #[cfg(feature = "tls")]
            {
                exporter = exporter.with_tls_config(tls_config.tls_config()?);
            }
            #[cfg(not(feature = "tls"))]
            {
                return Err(ConfigurationError::MissingFeature("tls"));
            }
        }
        if let Some(metadata) = self.metadata.clone() {
            exporter = exporter.with_metadata(metadata);
        }
        Ok(exporter)
    }

    pub fn exporter_from_env() -> opentelemetry_otlp::TonicExporterBuilder {
        let mut exporter = opentelemetry_otlp::new_exporter().tonic();
        exporter = exporter.with_env();
        exporter
    }
}

mod header_map_serde {
    use super::*;

    pub(crate) fn serialize<S>(map: &Option<MetadataMap>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        if map.as_ref().map(|x| x.is_empty()).unwrap_or(true) {
            return serializer.serialize_none();
        }

        let mut serializable_format =
            Vec::with_capacity(map.as_ref().map(|x| x.len()).unwrap_or(0));

        serializable_format.extend(map.iter().flat_map(|x| x.iter()).map(|key_and_value| {
            match key_and_value {
                KeyAndValueRef::Ascii(key, value) => {
                    let mut map = HashMap::with_capacity(1);
                    map.insert(key.as_str(), value.to_str().unwrap());
                    map
                }
                KeyAndValueRef::Binary(_, _) => todo!(),
            }
        }));

        serializable_format.serialize(serializer)
    }

    pub(crate) fn deserialize<'de, D>(deserializer: D) -> Result<Option<MetadataMap>, D::Error>
    where
        D: serde::de::Deserializer<'de>,
    {
        let serializable_format: Vec<HashMap<String, String>> =
            Deserialize::deserialize(deserializer)?;

        if serializable_format.is_empty() {
            return Ok(None);
        }

        let mut map = MetadataMap::new();

        for submap in serializable_format.into_iter() {
            for (key, value) in submap.into_iter() {
                let key = MetadataKey::from_bytes(key.as_bytes()).unwrap();
                map.append(key, value.parse().unwrap());
            }
        }

        Ok(Some(map))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn serialize_metadata_map() {
            let mut map = MetadataMap::new();
            map.append("foo", "bar".parse().unwrap());
            map.append("foo", "baz".parse().unwrap());
            map.append("bar", "foo".parse().unwrap());
            let mut buffer = Vec::new();
            let mut ser = serde_yaml::Serializer::new(&mut buffer);
            serialize(&Some(map), &mut ser).unwrap();
            insta::assert_snapshot!(std::str::from_utf8(&buffer).unwrap());
            let de = serde_yaml::Deserializer::from_slice(&buffer);
            deserialize(de).unwrap();
        }
    }
}
