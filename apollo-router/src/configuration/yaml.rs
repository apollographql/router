//! Reads configuration YAML structure that the parsers themselves discard: duplicate keys, which
//! they collapse, and the configuration path at a position in the text.

use std::collections::HashMap;
use std::collections::HashSet;

use indexmap::IndexMap;
use yaml_rust::Event;
use yaml_rust::parser::MarkedEventReceiver;
use yaml_rust::parser::Parser;
use yaml_rust::scanner::Marker;

use crate::configuration::ConfigurationError;

type Label = String;

#[derive(Clone, Debug)]
enum Value {
    Scalar,
    Sequence(Vec<Value>),
    Mapping(Option<Label>, IndexMap<Label, Value>),
}

/// The document's structure, enough to notice a key that appears twice in one mapping.
#[derive(Default, Debug)]
struct DuplicateKeys {
    anchors: HashMap<usize, Value>,
    current_label: Option<Label>,
    object_stack: Vec<(Option<Label>, Value, usize)>,
    duplicated_fields: HashSet<(Option<Label>, Label)>,
}

impl DuplicateKeys {
    fn end_container(&mut self) {
        let (label, v, id) = self.object_stack.pop().expect("imbalanced parse events");
        self.anchors.insert(id, v.clone());
        match (label, self.object_stack.last_mut()) {
            (Some(label), Some((_, Value::Mapping(current_label, mapping), _))) => {
                if let Some(_previous) = mapping.insert(label.clone(), v) {
                    self.duplicated_fields
                        .insert((current_label.clone(), label));
                }
            }
            (None, Some((_, Value::Sequence(sequence), _))) => {
                sequence.push(v);
            }
            _ => {}
        }
    }

    fn add_value(&mut self, v: String, id: usize) {
        match (self.current_label.take(), self.object_stack.last_mut()) {
            (Some(label), Some((_, Value::Mapping(current_label, mapping), _))) => {
                self.anchors.insert(id, Value::Scalar);
                if let Some(_previous) = mapping.insert(label.clone(), Value::Scalar) {
                    self.duplicated_fields
                        .insert((current_label.clone(), label));
                }
            }
            (None, Some((_, Value::Sequence(sequence), _))) => {
                self.anchors.insert(id, Value::Scalar);
                sequence.push(Value::Scalar);
            }
            (None, _) => self.current_label = Some(v),
            _ => tracing::warn!("labeled scalar without container in yaml"),
        }
    }

    fn add_alias_value(&mut self, v: Value) {
        match (self.current_label.take(), self.object_stack.last_mut()) {
            (Some(label), Some((_, Value::Mapping(_current_label, mapping), _))) => {
                mapping.insert(label, v);
            }
            (None, Some((_, Value::Sequence(sequence), _))) => {
                sequence.push(v);
            }
            _ => tracing::warn!("scalar without container in yaml"),
        }
    }
}

/// Rejects YAML that is malformed or repeats a key within one mapping.
pub(crate) fn check_duplicate_keys(source: &str) -> Result<(), ConfigurationError> {
    // Yaml parser doesn't support CRLF. Remove CRs.
    // https://github.com/chyh1990/yaml-rust/issues/165
    let source = source.replace('\r', "");
    let mut parser = Parser::new(source.chars());
    let mut loader = DuplicateKeys::default();
    parser
        .load(&mut loader, true)
        .map_err(|e| ConfigurationError::InvalidConfiguration {
            message: "could not parse yaml",
            error: e.to_string(),
        })?;

    // Detect duplicated keys in configuration file
    if !loader.duplicated_fields.is_empty() {
        let error = loader
            .duplicated_fields
            .iter()
            .map(|(parent_label, dup_label)| {
                let prefix = parent_label
                    .as_ref()
                    .map(|label| format!("{label}."))
                    .unwrap_or_default();
                format!("'{prefix}{dup_label}'")
            })
            .collect::<Vec<String>>()
            .join(", ");
        return Err(ConfigurationError::InvalidConfiguration {
            message: "duplicated keys detected in your yaml configuration",
            error,
        });
    }

    Ok(())
}

impl MarkedEventReceiver for DuplicateKeys {
    fn on_event(&mut self, ev: Event, _marker: Marker) {
        match ev {
            Event::Scalar(v, _style, id, _tag) => self.add_value(v, id),
            Event::SequenceStart(id) => {
                self.object_stack.push((
                    self.current_label.take(),
                    Value::Sequence(Vec::new()),
                    id,
                ));
            }
            Event::SequenceEnd => self.end_container(),
            Event::MappingStart(id) => {
                let current_label = self.current_label.take();
                self.object_stack.push((
                    current_label.clone(),
                    Value::Mapping(current_label, IndexMap::default()),
                    id,
                ));
            }
            Event::MappingEnd => self.end_container(),
            Event::Alias(id) => {
                if let Some(v) = self.anchors.get(&id) {
                    let cloned = v.clone();
                    self.add_alias_value(cloned);
                } else {
                    tracing::warn!("unresolved anchor in yaml");
                }
            }
            Event::DocumentStart => {}
            Event::DocumentEnd => {}
            _ => {}
        }
    }
}

#[cfg(test)]
mod test {
    use crate::configuration::yaml::check_duplicate_keys;

    #[test]
    fn test_duplicate_keys() {
        // DON'T reformat this. It'll change the test results
        let yaml = r#"test:
  a: 4
  b: 3
  a: 5
  c:
    dup: 5
    other: 3
    dup: 8
test:
  foo: bar
"#;
        let err = check_duplicate_keys(yaml).unwrap_err();
        match err {
            crate::configuration::ConfigurationError::InvalidConfiguration { message, error } => {
                assert_eq!(
                    message,
                    "duplicated keys detected in your yaml configuration"
                );
                // Can't do an assert on error because under the hood it uses an hashset then the order is not guaranteed
                let error_splitted: Vec<&str> = error.split(", ").collect();
                assert_eq!(error_splitted.len(), 3);
                assert!(error_splitted.contains(&"'test.a'"));
                assert!(error_splitted.contains(&"'test'"));
                assert!(error_splitted.contains(&"'c.dup'"));
            }
            _ => panic!("this error must be InvalidConfiguration variant"),
        }
    }
}
