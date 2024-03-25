use std::collections::HashMap;

use indexmap::IndexMap;
use indexmap::IndexSet;
use serde_json_bytes::ByteString;

use super::error::FileUploadError;
use super::Result as UploadResult;

type MapPerVariable = HashMap<String, MapPerFile>;
type MapPerFile = HashMap<String, Vec<Vec<String>>>;

pub(super) type MapFieldRaw = IndexMap<String, Vec<String>>;

#[derive(Debug)]
pub(super) struct MapField {
    pub(super) files_order: IndexSet<String>,
    pub(super) per_variable: MapPerVariable,
}

impl MapField {
    pub(super) fn new(map_field: MapFieldRaw) -> UploadResult<Self> {
        let mut files_order = IndexSet::new();
        let mut map_per_variable: MapPerVariable = HashMap::new();
        for (filename, paths) in map_field.into_iter() {
            for path in paths.into_iter() {
                let mut segments = path.split('.');
                let first_segment = segments.next();
                if first_segment != Some("variables") {
                    if first_segment
                        .and_then(|str| str.parse::<usize>().ok())
                        .is_some()
                    {
                        return Err(FileUploadError::BatchRequestAreNotSupported);
                    }
                    return Err(FileUploadError::InvalidPathInsideMapField(path));
                }
                let variable_path: Vec<String> = segments.map(str::to_owned).collect();
                let variable_name = variable_path.first().ok_or_else(|| {
                    FileUploadError::MissingVariableNameInsideMapField(path.clone())
                })?;

                map_per_variable
                    .entry(variable_name.to_owned())
                    .or_default()
                    .entry(filename.clone())
                    .or_default()
                    .push(variable_path);
            }
            files_order.insert(filename);
        }

        Ok(Self {
            files_order,
            per_variable: map_per_variable,
        })
    }

    pub(super) fn sugraph_map<'a>(
        &self,
        variable_names: impl IntoIterator<Item = &'a ByteString>,
    ) -> MapFieldRaw {
        let mut subgraph_map: MapFieldRaw = IndexMap::new();
        for variable_name in variable_names.into_iter() {
            let variable_name = variable_name.as_str();
            if let Some(variable_map) = self.per_variable.get(variable_name) {
                for (file, paths) in variable_map.iter() {
                    subgraph_map.insert(
                        file.clone(),
                        paths
                            .iter()
                            .map(|path| format!("variables.{}", path.join(".")))
                            .collect(),
                    );
                }
            }
        }
        subgraph_map.sort_by_cached_key(|file, _| self.files_order.get_index_of(file));
        subgraph_map
    }
}
