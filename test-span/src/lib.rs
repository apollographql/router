pub mod prelude {
    pub use crate::reexports::tracing_futures::WithSubscriber;
    pub use crate::reexports::tracing_subscriber::prelude::*;
    pub use crate::span_tests::{Layer, OwnedMetadata, RecordEntry, RecordedValue, Records, Span};
    pub use test_span_macro::test_span;
}

pub mod reexports {
    pub use daggy;
    pub use serde;
    pub use tracing;
    pub use tracing_futures;
    pub use tracing_subscriber;
}

mod span_tests {
    use ::daggy::petgraph::graph::DiGraph;
    use ::daggy::{Dag, NodeIndex};
    use ::serde::{Deserialize, Serialize};
    use ::std::collections::BTreeMap;
    use ::std::collections::HashMap;
    use ::std::sync::RwLock;
    use ::std::sync::{Arc, Mutex};
    use ::tracing::field::{Field, Visit};
    use ::tracing::span;
    use ::tracing::{Event, Metadata};
    use indexmap::IndexMap;

    pub type SpanEntry = Recorder;

    type FieldName = String;

    #[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
    pub enum RecordedValue {
        Error(String),
        Value(serde_json::Value),
        Debug(String),
    }

    #[derive(Default, Clone, Debug)]
    struct DumpVisitor(Vec<RecordEntry>);

    impl Visit for DumpVisitor {
        /// Visit a double-precision floating point value.
        fn record_f64(&mut self, field: &Field, value: f64) {
            self.0
                .push((field.name().to_string(), RecordedValue::Value(value.into())));
        }

        /// Visit a signed 64-bit integer value.
        fn record_i64(&mut self, field: &Field, value: i64) {
            self.0
                .push((field.name().to_string(), RecordedValue::Value(value.into())));
        }

        /// Visit an unsigned 64-bit integer value.
        fn record_u64(&mut self, field: &Field, value: u64) {
            self.0
                .push((field.name().to_string(), RecordedValue::Value(value.into())));
        }

        /// Visit a boolean value.
        fn record_bool(&mut self, field: &Field, value: bool) {
            self.0
                .push((field.name().to_string(), RecordedValue::Value(value.into())));
        }

        /// Visit a string value.
        fn record_str(&mut self, field: &Field, value: &str) {
            self.0
                .push((field.name().to_string(), RecordedValue::Value(value.into())));
        }

        /// Records a type implementing `Error`.
        ///
        /// <div class="example-wrap" style="display:inline-block">
        /// <pre class="ignore" style="white-space:normal;font:inherit;">
        /// <strong>Note</strong>: This is only enabled when the Rust standard library is
        /// present.
        /// </pre>
        #[cfg(feature = "std")]
        #[cfg_attr(docsrs, doc(cfg(feature = "std")))]
        fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
            self.0.push((
                field.name().to_string(),
                RecordedValue::Error(&format_args!("{}", value).into()),
            ));
        }

        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            self.0.push((
                field.name().to_string(),
                RecordedValue::Debug(format!("{:?}", value)),
            ));
        }
    }

    pub type RecordEntry = (FieldName, RecordedValue);

    #[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq, Hash)]

    pub struct OwnedFieldSet {
        names: Vec<String>,
    }

    impl From<&tracing::field::FieldSet> for OwnedFieldSet {
        fn from(fs: &tracing::field::FieldSet) -> Self {
            Self {
                names: fs.iter().map(|field| field.to_string()).collect(),
            }
        }
    }

    #[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq, Hash)]
    pub struct OwnedMetadata {
        pub name: String,

        /// The part of the system that the span that this metadata describes
        /// occurred in.
        pub target: String,

        /// The level of verbosity of the described span.
        // TODO[igni]: maybe put an enum here
        pub level: String,

        /// The name of the Rust module where the span occurred, or `None` if this
        /// could not be determined.
        pub module_path: Option<String>,

        /// The name of the source code file where the span occurred, or `None` if
        /// this could not be determined.
        #[serde(skip_serializing)]
        pub file: Option<String>,

        /// The line number in the source code file where the span occurred, or
        /// `None` if this could not be determined.
        #[serde(skip_serializing)]
        pub line: Option<u32>,

        /// The names of the key-value fields attached to the described span or
        /// event.
        pub fields: OwnedFieldSet,
    }

    impl From<&Metadata<'_>> for OwnedMetadata {
        fn from(md: &Metadata) -> Self {
            Self {
                name: md.name().to_string(),
                target: md.target().to_string(),
                level: md.level().to_string(),
                module_path: md.module_path().map(std::string::ToString::to_string),
                file: md.file().map(std::string::ToString::to_string),
                line: md.line(),
                fields: md.fields().into(),
            }
        }
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct Record {
        entries: Vec<RecordEntry>,
        metadata: OwnedMetadata,
    }

    impl Record {
        pub fn new(metadata: OwnedMetadata) -> Self {
            Self {
                entries: Default::default(),
                metadata,
            }
        }

        pub fn metadata(&self) -> OwnedMetadata {
            self.metadata.clone()
        }

        pub fn for_root() -> Self {
            Self {
                entries: Vec::new(),
                metadata: OwnedMetadata {
                    name: "root".to_string(),
                    ..Default::default()
                },
            }
        }

        pub fn push(&mut self, entry: RecordEntry) {
            self.entries.push(entry)
        }

        pub fn append(&mut self, mut entries: Vec<RecordEntry>) {
            self.entries.append(&mut entries)
        }
    }
    #[derive(Debug, Serialize, Deserialize, Clone, Default)]
    pub struct Records(Vec<Record>);

    impl Records {
        pub fn contains_message(&self, lookup: impl AsRef<str>) -> bool {
            self.contains_value("message", RecordedValue::Debug(lookup.as_ref().to_string()))
        }

        pub fn contains_value(&self, field_name: impl AsRef<str>, lookup: RecordedValue) -> bool {
            self.0.iter().any(|r| {
                r.entries
                    .iter()
                    .any(|(field, value)| field.as_str() == field_name.as_ref() && value == &lookup)
            })
        }

        pub fn merge(self, other: Records) -> Self {
            Self(self.0.into_iter().chain(other.0.into_iter()).collect())
        }
    }

    #[derive(Clone, Debug, Default)]
    pub struct Recorder {
        metadata: Option<OwnedMetadata>,
        visitor: DumpVisitor,
    }

    impl Recorder {
        pub fn attributes(&mut self, attributes: &span::Attributes<'_>) {
            self.metadata = Some(attributes.metadata().into());
            attributes.record(&mut self.visitor)
        }

        pub fn record(&mut self, record: &span::Record<'_>) {
            record.record(&mut self.visitor)
        }

        pub fn contents(&self) -> Record {
            let mut r = Record::new(self.metadata.clone().unwrap());
            r.append(self.visitor.0.clone());
            r
        }
    }

    #[derive(Debug, Default, Clone)]
    pub struct LogsRecorder {
        visitors: IndexMap<OwnedMetadata, DumpVisitor>,
    }

    impl LogsRecorder {
        pub fn event(&mut self, event: &Event<'_>) {
            event.record(self.visitors.entry(event.metadata().into()).or_default())
        }

        pub fn for_metadata(&mut self, metadata: OwnedMetadata) -> Record {
            let mut r = Record::new(metadata.clone());
            r.append(self.visitors.entry(metadata).or_default().0.clone());
            r
        }

        pub fn contents(&self) -> Records {
            Records(
                self.visitors
                    .iter()
                    .map(|(metadata, records)| {
                        let mut r = Record::new(metadata.clone());
                        r.append(records.0.clone());
                        r
                    })
                    .collect(),
            )
        }
    }

    #[derive(Debug)]
    pub struct Layer {
        current_ids: Mutex<Vec<span::Id>>,
        id_sequence: Arc<RwLock<Vec<Vec<span::Id>>>>,
        all_spans: Arc<Mutex<HashMap<u64, SpanEntry>>>,
        logs: Arc<Mutex<LogsRecorder>>,
    }

    impl Layer {
        pub fn new(
            id_sequence: Arc<RwLock<Vec<Vec<span::Id>>>>,
            all_spans: Arc<Mutex<HashMap<u64, SpanEntry>>>,
            logs: Arc<Mutex<LogsRecorder>>,
        ) -> Self {
            Self {
                current_ids: Default::default(),
                id_sequence,
                all_spans,
                logs,
            }
        }
    }

    #[derive(Debug, Serialize, Deserialize)]
    pub struct Span {
        id: usize,
        name: String,
        record: Record,
        children: BTreeMap<String, Span>,
    }

    impl Span {
        pub fn from(name: String, id: usize, record: Record) -> Self {
            Self {
                name,
                id,
                record,
                children: Default::default(),
            }
        }

        pub fn from_records(
            id_sequence: Vec<Vec<span::Id>>,
            mut all_logs: LogsRecorder,
            all_spans: HashMap<u64, SpanEntry>,
        ) -> Self {
            let mut dag_mapping = HashMap::new();

            let mut dag = Dag::new();

            let root = dag.add_node(0);

            for span_ids in id_sequence {
                let mut parent = root;

                for id in span_ids {
                    let (_, child_index) = dag.add_child(parent, (), id.into_u64());
                    parent = child_index;

                    let mut spans_record = all_spans
                        .get(&id.into_u64())
                        .map(std::clone::Clone::clone)
                        .expect("there should be a span recorder")
                        .contents();

                    let logs = all_logs.for_metadata(spans_record.metadata()).entries;

                    spans_record.append(logs);

                    dag_mapping.insert(
                        child_index.index(),
                        (
                            id.into_u64().try_into().expect("32 bits platform :/"),
                            spans_record,
                        ),
                    );
                }
            }

            SpanBuilder::new(dag.into_graph(), dag_mapping).into_span()
        }
    }

    struct SpanBuilder {
        graph: DiGraph<u64, ()>,
        spans: HashMap<usize, (usize, Record)>,
    }

    impl SpanBuilder {
        fn new(graph: DiGraph<u64, ()>, spans: HashMap<usize, (usize, Record)>) -> Self {
            Self { graph, spans }
        }

        fn into_span(self) -> Span {
            let mut root_span = Span::from("root".to_string(), 0, Record::for_root());

            self.dfs_insert(&mut root_span, NodeIndex::from(0));

            root_span
        }

        fn dfs_insert(&self, current_span: &mut Span, current_node: NodeIndex) {
            current_span.children = self
                .graph
                .neighbors(current_node)
                .map(|child_node| {
                    let (_, child_record) = self
                        .spans
                        .get(&child_node.index())
                        .expect("graph and hashmap are tied; qed");

                    let span_name = format!(
                        "{}::{}",
                        child_record.metadata.target, child_record.metadata.name
                    );

                    let span_key = format!("{} - {}", span_name, child_node.index());

                    let mut child_span =
                        Span::from(span_name, child_node.index(), child_record.clone());

                    self.dfs_insert(&mut child_span, child_node);
                    (span_key, child_span)
                })
                .collect();
        }
    }

    impl Layer {
        fn record(&self, id: span::Id, record: &span::Record<'_>) {
            self.all_spans
                .lock()
                .unwrap()
                .get_mut(&id.into_u64())
                .unwrap_or_else(|| panic!("no record for id {}", id.into_u64()))
                .record(record);
        }

        fn event(&self, event: &Event<'_>) {
            self.logs.lock().unwrap().event(event);
        }

        fn attributes(&self, id: span::Id, attributes: &span::Attributes<'_>) {
            self.all_spans
                .lock()
                .unwrap()
                .entry(id.into_u64())
                .or_default()
                .attributes(attributes);
        }

        fn enter_id(&self, id: span::Id) {
            self.current_ids.lock().unwrap().push(id);
        }

        fn exit_id(&self, id: span::Id) {
            {
                let mut current = self.current_ids.lock().unwrap();
                *current = current
                    .iter()
                    .take_while(|parent_id| parent_id != &&id)
                    .map(std::clone::Clone::clone)
                    .collect();
            }
        }

        fn close_id(&self, id: span::Id) {
            let mut with_close_id = self.current_span_list();
            with_close_id.push(id.clone());
            self.id_sequence.write().unwrap().push(with_close_id);
            self.exit_id(id);
        }

        fn current_span_list(&self) -> Vec<span::Id> {
            self.current_ids.lock().unwrap().clone()
        }
    }

    impl<S> tracing_subscriber::Layer<S> for Layer
    where
        S: tracing::Subscriber,
    {
        fn on_layer(&mut self, _subscriber: &mut S) {}

        fn register_callsite(
            &self,
            _metadata: &'static Metadata<'static>,
        ) -> tracing::subscriber::Interest {
            tracing::subscriber::Interest::always()
        }

        fn enabled(
            &self,
            _metadata: &Metadata<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) -> bool {
            true
        }

        fn on_new_span(
            &self,
            attrs: &span::Attributes<'_>,
            id: &span::Id,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            self.attributes(id.clone(), attrs)
        }

        fn max_level_hint(&self) -> Option<tracing::metadata::LevelFilter> {
            None
        }

        fn on_record(
            &self,
            span: &span::Id,
            values: &span::Record<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            self.record(span.clone(), values)
        }

        fn on_follows_from(
            &self,
            _span: &span::Id,
            _follows: &span::Id,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
        }

        fn on_event(&self, event: &Event<'_>, _ctx: tracing_subscriber::layer::Context<'_, S>) {
            self.event(event)
        }

        fn on_enter(&self, id: &span::Id, _ctx: tracing_subscriber::layer::Context<'_, S>) {
            self.enter_id(id.clone());
        }

        fn on_exit(&self, id: &span::Id, _ctx: tracing_subscriber::layer::Context<'_, S>) {
            self.exit_id(id.clone());
        }

        fn on_close(&self, id: span::Id, _ctx: tracing_subscriber::layer::Context<'_, S>) {
            self.close_id(id);
        }

        fn on_id_change(
            &self,
            _old: &span::Id,
            _new: &span::Id,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
        }
    }
}
