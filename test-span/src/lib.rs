//! the test span library provides you with two functions:
//!
//! `get_logs()` that returns [`prelude::Records`]
//!
//! `get_span()` that returns a [`prelude::Span`],
//! Which can be serialized and used with [insta](https://crates.io/crates/insta) for snapshot tests.
//!  Refer to the tests.rs file to see how it behaves.
//!
//! Example:
//! ```ignore
//! #[test_span]
//! async fn test_it_works() {
//!   futures::join!(do_stuff(), do_stuff())
//! }
//!
//! #[tracing::instrument(name = "do_stuff", level = "info")]
//! async fn do_stuff() -> u8 {
//!     // ...
//!     do_stuff2().await;
//! }
//!
//! #[tracing::instrument(
//!     name = "do_stuff2",
//!     target = "my_crate::an_other_target",
//!     level = "info"
//! )]
//! async fn do_stuff_2(number: u8) -> u8 {
//!     // ...
//! }
//! ```
//! ```text
//! `get_span()` will provide you with:
//!
//!             ┌──────┐
//!             │ root │
//!             └──┬───┘
//!                │
//!        ┌───────┴───────┐
//!        ▼               ▼
//!   ┌──────────┐   ┌──────────┐
//!   │ do_stuff │   │ do_stuff │
//!   └────┬─────┘   └─────┬────┘
//!        │               │
//!        │               │
//!        ▼               ▼
//!  ┌───────────┐   ┌───────────┐
//!  │ do_stuff2 │   │ do_stuff2 │
//!  └───────────┘   └───────────┘
//! ```

use once_cell::sync::Lazy;
use prelude::*;
use span_tests::Report;
use tracing::Level;
use tracing_subscriber::util::TryInitError;

static INIT: Lazy<Result<(), TryInitError>> =
    Lazy::new(|| tracing_subscriber::registry().with(Layer {}).try_init());

pub fn init() {
    Lazy::force(&INIT).as_ref().expect("couldn't set span-test subscriber as a default, maybe tracing has already been initialized somewhere else ?");
}

pub fn get_telemetry_for_root(
    root_id: &crate::reexports::tracing::Id,
    level: &Level,
) -> (Span, Records) {
    let report = Report::from_root(root_id.into_u64());

    (report.spans(level), report.logs(level))
}

pub fn get_spans_for_root(root_id: &crate::reexports::tracing::Id, level: &Level) -> Span {
    Report::from_root(root_id.into_u64()).spans(level)
}

pub fn get_logs_for_root(root_id: &crate::reexports::tracing::Id, level: &Level) -> Records {
    Report::from_root(root_id.into_u64()).logs(level)
}
pub mod prelude {
    pub use crate::reexports::tracing::{Instrument, Level};
    pub use crate::reexports::tracing_futures::WithSubscriber;
    pub use crate::reexports::tracing_subscriber::prelude::*;
    pub use crate::span_tests::{Layer, OwnedMetadata, RecordEntry, RecordedValue, Records, Span};
    pub use crate::{get_logs_for_root, get_spans_for_root, get_telemetry_for_root};
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
    use ::daggy::{Dag, NodeIndex};
    use ::serde::{Deserialize, Serialize};
    use ::std::collections::BTreeMap;
    use ::std::sync::{Arc, Mutex};
    use ::tracing::field::{Field, Visit};
    use ::tracing::span;
    use ::tracing::{Event, Metadata};
    use daggy::petgraph::graph::DefaultIx;
    use daggy::Walker;
    use indexmap::IndexMap;
    use once_cell::sync::Lazy;
    use std::collections::HashSet;
    use tracing_subscriber::layer::Context;

    type LazyMutex<T> = Lazy<Arc<Mutex<T>>>;

    pub(crate) static ALL_SPANS: LazyMutex<IndexMap<u64, Recorder>> = Lazy::new(Default::default);
    pub(crate) static ALL_LOGS: LazyMutex<LogsRecorder> = Lazy::new(Default::default);
    pub(crate) static ALL_DAGS: LazyMutex<IndexMap<u64, Dag<u64, ()>>> =
        Lazy::new(Default::default);
    pub(crate) static SPAN_ID_TO_ROOT_AND_NODE_INDEX: LazyMutex<
        IndexMap<u64, (u64, daggy::NodeIndex)>,
    > = Lazy::new(Default::default);

    type FieldName = String;

    #[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
    #[serde(untagged)]
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

        #[serde(skip_serializing)]
        /// span_id when available, used to match logs and spans when applicable
        pub span_id: Option<u64>,
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
                span_id: None,
            }
        }
    }

    impl OwnedMetadata {
        pub fn with_span_id(self, span_id: u64) -> Self {
            Self {
                span_id: Some(span_id),
                ..self
            }
        }
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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
    #[derive(Debug, Serialize, Deserialize, Clone, Default, PartialEq, Eq)]
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

        fn push(&mut self, record: Record) {
            self.0.push(record)
        }
    }

    #[derive(Clone, Debug, Default)]
    pub struct Recorder {
        metadata: Option<OwnedMetadata>,
        visitor: DumpVisitor,
    }

    impl Recorder {
        pub fn attributes(&mut self, span_id: tracing::Id, attributes: &span::Attributes<'_>) {
            let mut owned_metadata: OwnedMetadata = attributes.metadata().into();
            owned_metadata.span_id = Some(span_id.into_u64());
            self.metadata = Some(owned_metadata);
            attributes.record(&mut self.visitor)
        }

        pub fn record(&mut self, record: &span::Record<'_>) {
            record.record(&mut self.visitor)
        }

        pub fn contents(&self, level: &tracing::Level) -> Record {
            let mut r = Record::new(self.metadata.clone().unwrap());

            if &r
                .metadata
                .level
                .clone()
                .parse::<tracing::Level>()
                .expect("invalid level")
                <= level
            {
                r.append(self.visitor.0.clone());
            }
            r
        }
    }

    #[derive(Debug, Default, Clone)]
    pub struct LogsRecorder {
        visitors: IndexMap<OwnedMetadata, DumpVisitor>,
    }

    impl LogsRecorder {
        pub fn event(&mut self, current_span_id: Option<tracing::Id>, event: &Event<'_>) {
            let metadata = OwnedMetadata::from(event.metadata());
            let metadata = if let Some(id) = current_span_id {
                metadata.with_span_id(id.into_u64())
            } else {
                metadata
            };
            event.record(self.visitors.entry(metadata).or_default())
        }

        pub fn for_spans(&self, spans: HashSet<u64>) -> Self {
            Self {
                visitors: self
                    .visitors
                    .iter()
                    .filter_map(|(log_metadata, visitor)| match log_metadata.span_id {
                        Some(id) if spans.contains(&id) => {
                            Some((log_metadata.clone(), visitor.clone()))
                        }
                        _ => None,
                    })
                    .collect(),
            }
        }

        pub fn record_for_span_id_and_level(
            &self,
            span_id: u64,
            level: &tracing::Level,
        ) -> Vec<RecordEntry> {
            self.visitors
                .iter()
                .filter_map(|(log_metadata, visitor)| {
                    if &log_metadata
                        .level
                        .parse::<tracing::Level>()
                        .expect("invalid level")
                        <= level
                    {
                        match log_metadata.span_id {
                            Some(id) if id == span_id => Some(visitor.0.clone()),
                            _ => None,
                        }
                    } else {
                        None
                    }
                })
                .flatten()
                .collect()
        }
    }

    #[derive(Debug)]
    struct SpanGraph {}

    #[derive(Debug, Default)]
    pub struct Layer {}

    #[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
    pub struct Span {
        #[serde(skip_serializing)]
        id: u64,
        name: String,
        record: Record,
        children: BTreeMap<String, Span>,
    }

    impl Span {
        pub fn from(name: String, id: u64, record: Record) -> Self {
            Self {
                name,
                id,
                record,
                children: Default::default(),
            }
        }
    }

    pub struct Report {
        root_index: NodeIndex,
        root_id: u64,
        dag: Dag<u64, (), DefaultIx>,
        spans: IndexMap<u64, Recorder>,
        logs: LogsRecorder,
        node_to_id: IndexMap<NodeIndex, u64>,
    }

    impl Report {
        pub fn from_root(root_node: u64) -> Self {
            let id_to_node = SPAN_ID_TO_ROOT_AND_NODE_INDEX.lock().unwrap().clone();
            let (global_root, root_node_index) = id_to_node
                .get(&root_node)
                .map(std::clone::Clone::clone)
                .expect("couldn't find rood node");

            let node_to_id: IndexMap<NodeIndex, u64> = id_to_node
                .into_iter()
                .filter_map(|(key, (root, value))| (root == global_root).then(|| (value, key)))
                .collect();

            let relevant_spans = node_to_id.values().cloned().collect::<HashSet<_>>();

            let spans = ALL_SPANS
                .lock()
                .unwrap()
                .clone()
                .into_iter()
                .filter(|(span_id, _)| relevant_spans.contains(span_id))
                .collect();

            let logs = ALL_LOGS.lock().unwrap().for_spans(relevant_spans);

            let dag = ALL_DAGS
                .lock()
                .unwrap()
                .get(&global_root)
                .expect("no dag for root")
                .clone();

            Self {
                root_index: root_node_index,
                root_id: root_node,
                dag,
                spans,
                node_to_id,
                logs,
            }
        }

        pub fn logs(&self, level: &tracing::Level) -> Records {
            if let Some(recorder) = self.spans.get(&self.root_id) {
                let mut contents = recorder.contents(level);
                contents.append(self.logs.record_for_span_id_and_level(self.root_id, level));

                let mut records = Records(vec![contents]);

                for (_, child_node) in self.dag.children(self.root_index).iter(&self.dag) {
                    let child_id = self
                        .node_to_id
                        .get(&child_node)
                        .expect("couldn't find span id for node");

                    let mut child_record = self
                        .spans
                        .get(child_id)
                        .expect("graph and hashmap are tied; qed")
                        .contents(level);

                    if &child_record
                        .metadata
                        .level
                        .parse::<tracing::Level>()
                        .expect("invalid tracing level")
                        > level
                    {
                        continue;
                    }

                    child_record.append(self.logs.record_for_span_id_and_level(*child_id, level));

                    records.push(child_record.clone());
                }

                records
            } else {
                Default::default()
            }
        }

        pub fn spans(&self, level: &tracing::Level) -> Span {
            if let Some(recorder) = self.spans.get(&self.root_id) {
                let metadata = recorder
                    .metadata
                    .as_ref()
                    .map(std::clone::Clone::clone)
                    .expect("recorder without metadata");
                let span_name = format!("{}::{}", metadata.target, metadata.name);

                let mut root_span = Span::from(span_name, self.root_id, recorder.contents(level));

                self.dfs_span_insert(&mut root_span, self.root_index, level);

                root_span
            } else {
                Span::from("root".to_string(), 0, Record::for_root())
            }
        }

        fn dfs_span_insert(
            &self,
            current_span: &mut Span,
            current_node: NodeIndex,
            level: &tracing::Level,
        ) {
            current_span.children = self
                .dag
                .children(current_node)
                .iter(&self.dag)
                .filter_map(|(_, child_node)| {
                    let child_id = self
                        .node_to_id
                        .get(&child_node)
                        .expect("couldn't find span id for node");
                    let child_recorder = self
                        .spans
                        .get(child_id)
                        .expect("graph and hashmap are tied; qed");

                    let metadata = child_recorder
                        .metadata
                        .as_ref()
                        .map(std::clone::Clone::clone)
                        .expect("couldn't find metadata for child record");

                    if &metadata
                        .level
                        .parse::<tracing::Level>()
                        .expect("invalid tracing level")
                        > level
                    {
                        return None;
                    }

                    let span_name = format!("{}::{}", metadata.target, metadata.name);

                    let span_key = format!("{} - {}", span_name, child_node.index());

                    let mut contents = child_recorder.contents(level);
                    contents.append(self.logs.record_for_span_id_and_level(*child_id, level));

                    let mut child_span = Span::from(span_name, *child_id, contents);
                    self.dfs_span_insert(&mut child_span, child_node, level);

                    Some((span_key, child_span))
                })
                .collect();
        }
    }

    impl Layer {
        fn record(&self, id: span::Id, record: &span::Record<'_>) {
            ALL_SPANS
                .lock()
                .unwrap()
                .get_mut(&id.into_u64())
                .unwrap_or_else(|| panic!("no record for id {}", id.into_u64()))
                .record(record);
        }

        fn event(&self, event: &Event<'_>, ctx: Context<'_, impl tracing::Subscriber>) {
            let current_span = ctx.current_span();
            let current_span = current_span.id().map(std::clone::Clone::clone);
            ALL_LOGS.lock().unwrap().event(current_span, event);
        }

        fn attributes(
            &self,
            span_id: span::Id,
            attributes: &span::Attributes<'_>,
            parent_id: Option<span::Id>,
        ) {
            let raw_span_id = span_id.into_u64();

            if let Some(id) = parent_id {
                // We have a parent, we can store the span in the right DAG
                let raw_parent_id = id.into_u64();

                let mut id_to_node_index = SPAN_ID_TO_ROOT_AND_NODE_INDEX.lock().unwrap();

                let (root_span_id, parent_node_index) = id_to_node_index
                    .get(&raw_parent_id)
                    .map(std::clone::Clone::clone)
                    .unwrap_or_else(|| panic!("missing parent attributes for {}.", raw_parent_id));

                let (_, node_index) =
                    if let Some(span_dag) = ALL_DAGS.lock().unwrap().get_mut(&root_span_id) {
                        span_dag.add_child(parent_node_index, (), raw_span_id)
                    } else {
                        panic!("missing dag for root {}", root_span_id);
                    };

                id_to_node_index.insert(raw_span_id, (root_span_id, node_index));
            } else {
                // We're dealing with a root, let's create a new DAG
                let mut new_dag: Dag<u64, ()> = Default::default();
                let root_index = new_dag.add_node(raw_span_id);

                // The span is the root here
                SPAN_ID_TO_ROOT_AND_NODE_INDEX
                    .lock()
                    .unwrap()
                    .insert(raw_span_id, (raw_span_id, root_index));

                let mut all_dags = ALL_DAGS.lock().unwrap();
                all_dags.insert(raw_span_id, new_dag);
            }

            ALL_SPANS
                .lock()
                .unwrap()
                .entry(raw_span_id)
                .or_default()
                .attributes(span_id, attributes);
        }
    }

    impl<S> tracing_subscriber::Layer<S> for Layer
    where
        S: tracing::Subscriber,
    {
        fn register_callsite(
            &self,
            _metadata: &'static Metadata<'static>,
        ) -> tracing::subscriber::Interest {
            tracing::subscriber::Interest::always()
        }

        fn on_new_span(
            &self,
            attrs: &span::Attributes<'_>,
            id: &span::Id,
            ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            let maybe_parent_id = attrs
                .parent()
                .map(std::clone::Clone::clone)
                .or_else(|| ctx.current_span().id().map(std::clone::Clone::clone));

            self.attributes(id.clone(), attrs, maybe_parent_id)
        }

        fn on_record(
            &self,
            span: &span::Id,
            values: &span::Record<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            self.record(span.clone(), values)
        }

        fn on_event(&self, event: &Event<'_>, ctx: tracing_subscriber::layer::Context<'_, S>) {
            self.event(event, ctx)
        }
    }
}
