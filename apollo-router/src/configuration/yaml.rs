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

/// The dotted path of the innermost node at `byte_offset` in `source`, such as
/// `apq.router.cache.redis.timeout`, or `None` at the document root. Sequence items appear as
/// their index.
pub(crate) fn path_at(source: &str, byte_offset: usize) -> Option<String> {
    enum Container {
        /// The key whose value is being read, once it has been seen.
        Mapping(Option<String>),
        Sequence(usize),
    }

    #[derive(Default)]
    struct Paths {
        /// Each open container, and whether it added a segment to `path`.
        containers: Vec<(Container, bool)>,
        path: Vec<String>,
        /// Where each node starts, as a character index, and its path.
        starts: Vec<(usize, String)>,
    }

    impl Paths {
        /// The segment for a node that starts now, or `None` when the node is a mapping key.
        fn segment(&mut self, key: Option<&str>) -> Option<Option<String>> {
            match self.containers.last_mut() {
                None => Some(None),
                Some((Container::Mapping(current), _)) => match current.take() {
                    Some(key) => Some(Some(key)),
                    None => {
                        *current = key.map(str::to_string);
                        None
                    }
                },
                Some((Container::Sequence(next), _)) => {
                    *next += 1;
                    Some(Some((*next - 1).to_string()))
                }
            }
        }

        fn record(&mut self, index: usize, segment: Option<&String>) {
            let path = self.path.iter().chain(segment).cloned().collect::<Vec<_>>();
            self.starts.push((index, path.join(".")));
        }
    }

    impl MarkedEventReceiver for Paths {
        fn on_event(&mut self, event: Event, marker: Marker) {
            match event {
                Event::Scalar(value, ..) => match self.segment(Some(&value)) {
                    Some(segment) => self.record(marker.index(), segment.as_ref()),
                    // A mapping key starts at its own path.
                    None => self.record(marker.index(), Some(&value)),
                },
                Event::Alias(_) => {
                    if let Some(segment) = self.segment(None) {
                        self.record(marker.index(), segment.as_ref());
                    }
                }
                Event::SequenceStart(_) | Event::MappingStart(_) => {
                    let segment = self.segment(None).flatten();
                    self.record(marker.index(), segment.as_ref());
                    let pushed = segment.is_some();
                    self.path.extend(segment);
                    let container = if matches!(event, Event::SequenceStart(_)) {
                        Container::Sequence(0)
                    } else {
                        Container::Mapping(None)
                    };
                    self.containers.push((container, pushed));
                }
                Event::SequenceEnd | Event::MappingEnd => {
                    if let Some((_, true)) = self.containers.pop() {
                        self.path.pop();
                    }
                }
                _ => {}
            }
        }
    }

    let prefix = source.get(..byte_offset)?;
    let index = prefix.replace('\r', "").chars().count();
    let source = source.replace('\r', "");
    let mut paths = Paths::default();
    Parser::new(source.chars()).load(&mut paths, true).ok()?;
    paths
        .starts
        .into_iter()
        .rfind(|(start, _)| *start <= index)
        .map(|(_, path)| path)
        .filter(|path| !path.is_empty())
}

#[cfg(test)]
mod test {
    use crate::configuration::yaml::check_duplicate_keys;
    use crate::configuration::yaml::path_at;

    #[test]
    fn path_at_names_the_node_at_an_offset() {
        let source = "apq:\n  router:\n    urls:\n    - redis://a\n    - redis://b\n    timeout: 5s\nlimits: {}\n";
        let at = |needle: &str| path_at(source, source.find(needle).unwrap());

        assert_eq!(at("router").as_deref(), Some("apq.router"));
        assert_eq!(at("redis://b").as_deref(), Some("apq.router.urls.1"));
        assert_eq!(at("5s").as_deref(), Some("apq.router.timeout"));
        assert_eq!(at("timeout").as_deref(), Some("apq.router.timeout"));
        assert_eq!(at("{}").as_deref(), Some("limits"));
        assert_eq!(path_at(source, 0).as_deref(), Some("apq"));
    }

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
