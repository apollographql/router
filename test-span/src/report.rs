use ::daggy::{Dag, NodeIndex};
use ::serde::{Deserialize, Serialize};
use daggy::petgraph::graph::DefaultIx;
use daggy::Walker;
use indexmap::IndexMap;
use linked_hash_map::LinkedHashMap;
use once_cell::sync::Lazy;
use std::collections::HashSet;

use crate::layer::{ALL_LOGS, ALL_SPANS, SPAN_ID_TO_ROOT_AND_NODE_INDEX};
use crate::log::LogsRecorder;
use crate::record::{Record, RecordValue, RecordWithMetadata, Recorder};
use crate::LazyMutex;

pub(crate) static ALL_DAGS: LazyMutex<IndexMap<u64, Dag<u64, ()>>> = Lazy::new(Default::default);

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Span {
    #[serde(skip_serializing)]
    id: u64,
    name: String,
    record: RecordWithMetadata,
    children: LinkedHashMap<String, Span>,
}

impl Span {
    pub fn from(name: String, id: u64, record: RecordWithMetadata) -> Self {
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

            let mut records: Vec<_> = contents.entries().cloned().collect();

            self.dfs_logs_insert(&mut records, self.root_index, level);

            Records::new(records)
        } else {
            Default::default()
        }
    }

    pub fn spans(&self, level: &tracing::Level) -> Span {
        if let Some(recorder) = self.spans.get(&self.root_id) {
            let metadata = recorder
                .metadata()
                .as_ref()
                .map(std::clone::Clone::clone)
                .expect("recorder without metadata");
            let span_name = format!("{}::{}", metadata.target, metadata.name);

            let mut root_span = Span::from(span_name, self.root_id, recorder.contents(level));

            self.dfs_span_insert(&mut root_span, self.root_index, level);

            root_span
        } else {
            Span::from("root".to_string(), 0, RecordWithMetadata::for_root())
        }
    }

    fn dfs_logs_insert(
        &self,
        records: &mut Vec<Record>,
        current_node: NodeIndex,
        level: &tracing::Level,
    ) {
        for child_node in self.sorted_children(current_node) {
            let child_id = self
                .node_to_id
                .get(&child_node)
                .expect("couldn't find span id for node");

            let mut child_record = self
                .spans
                .get(child_id)
                .expect("graph and hashmap are tied; qed")
                .contents(level);

            child_record.append(self.logs.record_for_span_id_and_level(*child_id, level));
            records.extend(child_record.entries().cloned().into_iter());
            self.dfs_logs_insert(records, child_node, level);
        }
    }

    fn dfs_span_insert(
        &self,
        current_span: &mut Span,
        current_node: NodeIndex,
        level: &tracing::Level,
    ) {
        current_span.children = self
            .sorted_children(current_node)
            .filter_map(|child_node| {
                let child_id = self
                    .node_to_id
                    .get(&child_node)
                    .expect("couldn't find span id for node");
                let child_recorder = self
                    .spans
                    .get(child_id)
                    .expect("graph and hashmap are tied; qed");

                let metadata = child_recorder
                    .metadata()
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

    fn sorted_children(&self, node: NodeIndex) -> impl Iterator<Item = NodeIndex> {
        let mut children = self
            .dag
            .children(node)
            .iter(&self.dag)
            .map(|(_, node)| node)
            .collect::<Vec<_>>();

        children.sort();

        children.into_iter()
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Default, PartialEq, Eq)]
pub struct Records(Vec<Record>);

impl Records {
    pub fn new(records: Vec<Record>) -> Self {
        Self(records)
    }
    pub fn contains_message(&self, lookup: impl AsRef<str>) -> bool {
        self.contains_value("message", RecordValue::Debug(lookup.as_ref().to_string()))
    }

    pub fn contains_value(&self, field_name: impl AsRef<str>, lookup: RecordValue) -> bool {
        self.0
            .iter()
            .any(|(field, value)| field.as_str() == field_name.as_ref() && value == &lookup)
    }

    pub fn push(&mut self, record: Record) {
        self.0.push(record)
    }
}
