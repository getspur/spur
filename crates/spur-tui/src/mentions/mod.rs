pub mod code_graph;
pub mod entry;
pub mod file_source;
pub(crate) mod hint;
pub mod issue_search;
pub mod issue_source;
pub mod registry;
pub mod worker_source;

pub use code_graph::source::CodeGraphMentionSource;
pub use entry::{MentionEntry, MentionKind, MentionSource};
pub use issue_source::{IssueMentionDescriptor, IssueMentionSource};
pub use registry::{CompletionScope, MentionRegistry, CODE_GRAPH_INDEX_ENV};
pub use spur_graph::{
    CodeMentionKind, CodeMentionPayload, CodeMentionValidationSpec, GraphIndexArtifact,
};
pub use worker_source::{WorkerMentionDescriptor, WorkerMentionSource};
