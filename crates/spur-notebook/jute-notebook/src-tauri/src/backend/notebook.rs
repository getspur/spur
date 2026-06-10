//! JSON schema for the Jupyter notebook `.ipynb` file format and Jute's
//! extensions.
//!
//! This file is based on the official [nbformat v4].
//!
//! [nbformat v4]: https://github.com/jupyter/nbformat/blob/v5.10.4/nbformat/v4/nbformat.v4.schema.json

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use ts_rs::TS;

/// Represents the root structure of a Jupyter Notebook file.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, TS)]
pub struct NotebookRoot {
    /// Root-level metadata of the notebook.
    pub metadata: NotebookMetadata,

    /// Notebook format (minor number). Incremented for backward-compatible
    /// changes.
    pub nbformat_minor: u8,

    /// Notebook format (major number). Incremented for incompatible changes.
    pub nbformat: u8,

    /// Array of cells in the notebook.
    pub cells: Vec<Cell>,
}

/// Root-level metadata for the notebook.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, TS)]
pub struct NotebookMetadata {
    /// Kernel information.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub kernelspec: Option<KernelSpec>,

    /// Programming language information.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub language_info: Option<LanguageInfo>,

    /// Original notebook format before conversion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub orig_nbformat: Option<u8>,

    /// Title of the notebook document.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub title: Option<String>,

    /// Authors of the notebook document.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub authors: Option<Vec<Author>>,

    /// jute-deck notebook-level metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub jute_deck: Option<JuteDeckNotebookMetadata>,

    /// Additional unrecognized attributes in metadata.
    #[serde(flatten)]
    #[ts(skip)]
    pub other: Map<String, Value>,
}

/// Kernel specification metadata.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, TS)]
pub struct KernelSpec {
    /// Name of the kernel specification.
    pub name: String,

    /// Display name of the kernel.
    pub display_name: String,

    /// Additional unrecognized attributes in kernel specification.
    #[serde(flatten)]
    #[ts(skip)]
    pub other: Map<String, Value>,
}

/// Programming language information.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, TS)]
pub struct LanguageInfo {
    /// Programming language name.
    pub name: String,

    /// `CodeMirror` mode to use for the language.
    #[ts(optional)]
    pub codemirror_mode: Option<CodeMirrorMode>,

    /// File extension for files in this language.
    #[ts(optional)]
    pub file_extension: Option<String>,

    /// MIME type for files in this language.
    #[ts(optional)]
    pub mimetype: Option<String>,

    /// Pygments lexer for syntax highlighting.
    #[ts(optional)]
    pub pygments_lexer: Option<String>,

    /// Additional unrecognized attributes in language information.
    #[serde(flatten)]
    #[ts(skip)]
    pub other: Map<String, Value>,
}

/// Represents the `CodeMirror` mode, which could be a string or a nested object.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, TS)]
#[serde(untagged)]
pub enum CodeMirrorMode {
    /// String representation of the `CodeMirror` mode.
    String(String),
    /// Nested object representation of the `CodeMirror` mode.
    Object(BTreeMap<String, Value>),
}

/// Author information for the notebook document.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, TS)]
pub struct Author {
    /// Name of the author.
    #[ts(optional)]
    pub name: Option<String>,

    /// Additional unrecognized attributes for authors.
    #[serde(flatten)]
    #[ts(skip)]
    pub other: Map<String, Value>,
}

/// Represents a notebook cell, which can be a raw, markdown, or code cell.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, TS)]
#[serde(tag = "cell_type", rename_all = "snake_case")]
pub enum Cell {
    /// Raw cell type.
    Raw(RawCell),

    /// Markdown cell type.
    Markdown(MarkdownCell),

    /// Code cell type.
    Code(CodeCell),
}

/// Raw cell in the notebook.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, TS)]
pub struct RawCell {
    /// Identifier of the cell.
    #[ts(optional)]
    pub id: Option<String>,

    /// Metadata for the cell.
    pub metadata: CellMetadata,

    /// Content of the cell.
    pub source: MultilineString,

    /// Attachments (e.g., images) in the cell.
    #[ts(optional)]
    pub attachments: Option<CellAttachments>,
}

/// Markdown cell in the notebook.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, TS)]
pub struct MarkdownCell {
    /// Identifier of the cell.
    #[ts(optional)]
    pub id: Option<String>,

    /// Metadata for the cell.
    pub metadata: CellMetadata,

    /// Content of the cell.
    pub source: MultilineString,

    /// Attachments (e.g., images) in the cell.
    #[ts(optional)]
    pub attachments: Option<CellAttachments>,
}

/// Code cell in the notebook.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, TS)]
pub struct CodeCell {
    /// Identifier of the cell.
    #[ts(optional)]
    pub id: Option<String>,

    /// Metadata for the cell.
    pub metadata: CellMetadata,

    /// Content of the cell.
    pub source: MultilineString,

    /// Execution count of the cell (null if not executed).
    pub execution_count: Option<u32>,

    /// Outputs from executing the cell.
    pub outputs: Vec<Output>,
}

/// Metadata for a cell.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, TS)]
pub struct CellMetadata {
    /// SPUR-managed cell metadata.
    #[ts(optional)]
    pub spur: Option<SpurCellMetadata>,

    /// jute-deck per-cell metadata (layout, hidden, `speaker_notes`, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub jute_deck: Option<JuteDeckCellMetadata>,

    /// Additional unrecognized attributes in cell metadata.
    #[serde(flatten)]
    #[ts(skip)]
    pub other: Map<String, Value>,
}

/// SPUR-managed metadata persisted under `cell.metadata.spur`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, TS)]
pub struct SpurCellMetadata {
    /// Per-cell monotonic content version.
    #[ts(type = "number")]
    pub version: u64,

    /// Agent that last edited the cell through notebook MCP tools.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub last_edited_by: Option<String>,

    /// Marks the SPUR-managed datasource setup cell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub datasource_setup: Option<bool>,

    /// Reactive DAG wiring metadata for this cell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub dag: Option<CellDagMetadata>,

    /// Language label used to route this code cell to a kernel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub code_type: Option<CodeType>,

    /// Frontend-cell declaration used by App mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub frontend: Option<FrontendCellMetadata>,
}

/// App-mode frontend declaration persisted under `cell.metadata.spur.frontend`.
///
/// Advisory props are intentionally not persisted; App mode keeps the durable
/// declaration to `kind`, `binds`, and `emits`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, TS)]
#[ts(export)]
pub struct FrontendCellMetadata {
    /// Frontend renderer kind, such as `html`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub kind: Option<String>,

    /// DAG ports this frontend cell reads.
    #[serde(default)]
    pub binds: Vec<String>,

    /// DAG ports this frontend cell emits.
    #[serde(default)]
    pub emits: Vec<String>,
}

/// Per-cell code language label persisted under `cell.metadata.spur.code_type`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum CodeType {
    /// Python code routed to the `python3` kernelspec.
    Python,
    /// JavaScript code routed to the `deno` kernelspec.
    Javascript,
    /// Rust code routed to the `evcxr` kernelspec.
    Rust,
    /// Go code routed to the `gonb` kernelspec.
    Go,
}

/// Return the kernelspec name for a per-cell code type.
pub fn kernelspec_for(code_type: CodeType) -> &'static str {
    match code_type {
        CodeType::Python => "python3",
        CodeType::Javascript => "deno",
        CodeType::Rust => "evcxr",
        CodeType::Go => "gonb",
    }
}

/// Return the per-cell code type for a kernelspec name.
pub fn code_type_for_spec(spec_name: &str) -> Option<CodeType> {
    match spec_name {
        "python3" => Some(CodeType::Python),
        "deno" => Some(CodeType::Javascript),
        "evcxr" => Some(CodeType::Rust),
        "gonb" => Some(CodeType::Go),
        _ => None,
    }
}

/// Reactive DAG metadata persisted under `cell.metadata.spur.dag`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub struct CellDagMetadata {
    /// Ports this cell produces.
    #[serde(default)]
    pub produces: Vec<PortSpec>,

    /// Ports this cell consumes.
    #[serde(default)]
    pub consumes: Vec<String>,

    /// Source port that feeds this cell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub source: Option<DagSource>,
}

/// Produced DAG port descriptor.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub struct PortSpec {
    /// Port identifier.
    pub port: String,

    /// Representation type for this port.
    pub repr: String,

    /// Optional display label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub display: Option<String>,
}

/// Source descriptor for a consumed DAG port.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub struct DagSource {
    /// Source kind.
    pub kind: String,

    /// Source port identifier.
    pub port: String,
}

/// jute-deck per-cell metadata.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub struct JuteDeckCellMetadata {
    /// Explicit slide layout; omitted = inferred from cell content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub layout: Option<JuteDeckLayout>,

    /// Skip this cell in deck/present mode while keeping it in the notebook.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub hidden: Option<bool>,

    /// Markdown speaker notes shown only via the S key overlay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub speaker_notes: Option<String>,

    /// Per-slide theme override (theme id from the notebook-level theme list).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub theme_override: Option<String>,

    /// Bullet-by-bullet reveal in present mode (markdown bullets only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub fragments: Option<bool>,

    /// Per-slide background color or image URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub background: Option<String>,
}

/// Explicit slide-layout values.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum JuteDeckLayout {
    /// Infer the layout from cell content.
    Auto,
    /// Title slide.
    Title,
    /// Section divider slide.
    Section,
    /// General markdown/content slide.
    Content,
    /// Bullet-list slide.
    Bullets,
    /// Source-code slide.
    Code,
    /// Output-only slide.
    Output,
    /// Code and output slide.
    CodeOutput,
    /// Two-column slide.
    TwoCol,
    /// Image-focused slide.
    Image,
    /// Blank/custom slide.
    Blank,
}

/// jute-deck notebook-level metadata.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub struct JuteDeckNotebookMetadata {
    /// Theme id; defaults to "minimal-light".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub theme: Option<String>,

    /// Slide aspect ratio; defaults to "16:9".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub aspect: Option<String>,

    /// Deck title override (defaults to filename).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub title: Option<String>,

    /// Deck author display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub author: Option<String>,
}

/// Attachments for a cell, represented as MIME bundles keyed by filenames.
pub type CellAttachments = BTreeMap<String, MimeBundle>;

/// MIME bundle for representing various types of data.
pub type MimeBundle = BTreeMap<String, Value>;

/// Represents a string or array of strings (multiline).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, TS)]
#[serde(untagged)]
pub enum MultilineString {
    /// Single-line string.
    Single(String),

    /// Multi-line array of strings.
    Multi(Vec<String>),
}

impl From<MultilineString> for String {
    fn from(m: MultilineString) -> Self {
        match m {
            MultilineString::Single(s) => s,
            MultilineString::Multi(v) if v.len() == 1 => v.into_iter().next().unwrap_or_default(),
            MultilineString::Multi(v) => v.join(""),
        }
    }
}

impl MultilineString {
    /// Convert a string to a multiline string, mimicking Jupyter.
    ///
    /// Usually, we could just use `MultilineString::Single`, but Jupyter's
    /// behavior is to always return an array, so we respect that. It also
    /// breaks strings after newline characters.
    pub fn normalize(&self) -> Self {
        let value = match self {
            Self::Single(s) => s,
            Self::Multi(v) => &v.join(""),
        };

        let mut lines = Vec::new();
        let mut remaining = &value[..];
        while !remaining.is_empty() {
            let next_break = remaining.find('\n').map_or(remaining.len(), |i| i + 1);
            lines.push(remaining[..next_break].to_string());
            remaining = &remaining[next_break..];
        }
        Self::Multi(lines)
    }
}

/// Output from executing a code cell.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, TS)]
#[serde(tag = "output_type", rename_all = "snake_case")]
pub enum Output {
    /// Execution result output.
    ExecuteResult(OutputExecuteResult),

    /// Display data output.
    DisplayData(OutputDisplayData),

    /// Stream output.
    Stream(OutputStream),

    /// Error output.
    Error(OutputError),
}

/// Result of executing a code cell.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, TS)]
pub struct OutputExecuteResult {
    /// Execution count of the result.
    pub execution_count: Option<u32>,

    /// Data returned by the execution.
    pub data: MimeBundle,

    /// Metadata associated with the result.
    pub metadata: OutputMetadata,

    /// Additional unrecognized attributes in execution results.
    #[serde(flatten)]
    #[ts(skip)]
    pub other: Map<String, Value>,
}

/// Display data output.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, TS)]
pub struct OutputDisplayData {
    /// Data to display.
    pub data: MimeBundle,

    /// Metadata associated with the display data.
    pub metadata: OutputMetadata,

    /// Additional unrecognized attributes in display data.
    #[serde(flatten)]
    #[ts(skip)]
    pub other: Map<String, Value>,
}

/// Stream output.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, TS)]
pub struct OutputStream {
    /// Name of the stream (e.g., stdout or stderr).
    pub name: String,

    /// Text content of the stream.
    pub text: MultilineString,

    /// Additional unrecognized attributes in stream output.
    #[serde(flatten)]
    #[ts(skip)]
    pub other: Map<String, Value>,
}

/// Error output.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, TS)]
pub struct OutputError {
    /// Name of the error.
    pub ename: String,

    /// Value or message of the error.
    pub evalue: String,

    /// Traceback of the error.
    pub traceback: Vec<String>,

    /// Additional unrecognized attributes in error output.
    #[serde(flatten)]
    #[ts(skip)]
    pub other: Map<String, Value>,
}

/// Metadata associated with outputs.
pub type OutputMetadata = BTreeMap<String, Value>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_notebook() {
        let json = r#"
            {
                "metadata": {
                    "kernelspec": {
                        "name": "python3",
                        "display_name": "Python 3"
                    },
                    "language_info": {
                        "name": "python",
                        "codemirror_mode": {
                            "name": "ipython",
                            "version": 3
                        },
                        "file_extension": ".py",
                        "mimetype": "text/x-python",
                        "pygments_lexer": "ipython3",
                        "version": "3.8.5",
                        "nbconvert_exporter": "python"
                    },
                    "orig_nbformat": 4,
                    "title": "Example Notebook",
                    "authors": [
                        {
                            "name": "Alice"
                        },
                        {
                            "name": "Bob"
                        }
                    ],
                    "custom": "metadata"
                },
                "nbformat_minor": 4,
                "nbformat": 4,
                "cells": [
                    {
                        "cell_type": "code",
                        "id": "cell-1",
                        "metadata": {
                            "custom": "metadata"
                        },
                        "source": "print('Hello, world!')",
                        "execution_count": 1,
                        "outputs": [
                            {
                                "output_type": "execute_result",
                                "execution_count": 1,
                                "data": {
                                    "text/plain": "Hello, world!"
                                },
                                "metadata": {
                                    "custom": "metadata"
                                }
                            }
                        ]
                    }
                ]
            }
        "#;

        let notebook: NotebookRoot = serde_json::from_str(json).unwrap();
        assert_eq!(
            notebook.metadata.kernelspec.as_ref().unwrap().name,
            "python3"
        );
        assert_eq!(
            notebook.metadata.language_info.as_ref().unwrap().name,
            "python"
        );
        assert_eq!(notebook.metadata.orig_nbformat, Some(4));
        assert_eq!(
            notebook.metadata.title.as_ref().unwrap(),
            "Example Notebook"
        );
        assert_eq!(
            notebook.metadata.authors.as_ref().unwrap()[0]
                .name
                .as_ref()
                .unwrap(),
            "Alice"
        );
        assert_eq!(
            notebook.metadata.authors.as_ref().unwrap()[1]
                .name
                .as_ref()
                .unwrap(),
            "Bob"
        );
        assert_eq!(notebook.metadata.other.get("custom").unwrap(), "metadata");
        assert_eq!(notebook.nbformat_minor, 4);
        assert_eq!(notebook.nbformat, 4);
        assert_eq!(notebook.cells.len(), 1);
    }

    #[test]
    fn cell_id_and_spur_version_survive_round_trip() {
        let json = r#"
            {
                "metadata": {},
                "nbformat_minor": 5,
                "nbformat": 4,
                "cells": [
                    {
                        "cell_type": "code",
                        "id": "550e8400-e29b-41d4-a716-446655440000",
                        "metadata": {
                            "spur": {
                                "version": 7
                            }
                        },
                        "source": "x = 1",
                        "execution_count": null,
                        "outputs": []
                    }
                ]
            }
        "#;

        let notebook: NotebookRoot = serde_json::from_str(json).unwrap();
        let Cell::Code(cell) = &notebook.cells[0] else {
            panic!("expected a code cell");
        };
        assert_eq!(
            cell.id.as_deref(),
            Some("550e8400-e29b-41d4-a716-446655440000")
        );
        assert_eq!(cell.metadata.spur.as_ref().unwrap().version, 7);

        let serialized = serde_json::to_value(&notebook).unwrap();
        assert_eq!(
            serialized["cells"][0]["id"],
            "550e8400-e29b-41d4-a716-446655440000"
        );
        assert_eq!(serialized["cells"][0]["metadata"]["spur"]["version"], 7);
    }

    #[test]
    fn spur_dag_metadata_survives_round_trip() {
        let json = r#"
            {
                "metadata": {},
                "nbformat_minor": 5,
                "nbformat": 4,
                "cells": [
                    {
                        "cell_type": "code",
                        "id": "550e8400-e29b-41d4-a716-446655440000",
                        "metadata": {
                            "spur": {
                                "version": 7,
                                "dag": {
                                    "produces": [
                                        {
                                            "port": "sales",
                                            "repr": "dataframe",
                                            "display": "Sales"
                                        }
                                    ],
                                    "consumes": ["config"],
                                    "source": {
                                        "kind": "cell",
                                        "port": "raw"
                                    }
                                }
                            }
                        },
                        "source": "x = 1",
                        "execution_count": null,
                        "outputs": []
                    }
                ]
            }
        "#;

        let notebook: NotebookRoot = serde_json::from_str(json).unwrap();
        let Cell::Code(cell) = &notebook.cells[0] else {
            panic!("expected a code cell");
        };
        let dag = cell.metadata.spur.as_ref().unwrap().dag.as_ref().unwrap();
        assert_eq!(dag.produces[0].port, "sales");
        assert_eq!(dag.produces[0].repr, "dataframe");
        assert_eq!(dag.produces[0].display.as_deref(), Some("Sales"));
        assert_eq!(dag.consumes, vec!["config"]);
        assert_eq!(dag.source.as_ref().unwrap().kind, "cell");
        assert_eq!(dag.source.as_ref().unwrap().port, "raw");

        let serialized = serde_json::to_value(&notebook).unwrap();
        assert_eq!(
            serialized["cells"][0]["metadata"]["spur"]["dag"]["produces"][0]["port"],
            "sales"
        );
        assert_eq!(
            serialized["cells"][0]["metadata"]["spur"]["dag"]["source"]["port"],
            "raw"
        );
    }

    #[test]
    fn spur_code_type_survives_round_trip_and_omits_when_absent() {
        let json = r#"
            {
                "metadata": {},
                "nbformat_minor": 5,
                "nbformat": 4,
                "cells": [
                    {
                        "cell_type": "code",
                        "id": "550e8400-e29b-41d4-a716-446655440000",
                        "metadata": {
                            "spur": {
                                "version": 7,
                                "code_type": "javascript"
                            }
                        },
                        "source": "await Promise.resolve(1)",
                        "execution_count": null,
                        "outputs": []
                    },
                    {
                        "cell_type": "code",
                        "id": "550e8400-e29b-41d4-a716-446655440001",
                        "metadata": {
                            "spur": {
                                "version": 8
                            }
                        },
                        "source": "x = 1",
                        "execution_count": null,
                        "outputs": []
                    }
                ]
            }
        "#;

        let notebook: NotebookRoot = serde_json::from_str(json).unwrap();
        let serialized = serde_json::to_value(&notebook).unwrap();
        assert_eq!(
            serialized["cells"][0]["metadata"]["spur"]["code_type"],
            "javascript"
        );
        assert!(serialized["cells"][1]["metadata"]["spur"]
            .as_object()
            .unwrap()
            .get("code_type")
            .is_none());

        assert_eq!(kernelspec_for(CodeType::Python), "python3");
        assert_eq!(kernelspec_for(CodeType::Javascript), "deno");
        assert_eq!(kernelspec_for(CodeType::Rust), "evcxr");
        assert_eq!(code_type_for_spec("python3"), Some(CodeType::Python));
        assert_eq!(code_type_for_spec("deno"), Some(CodeType::Javascript));
        assert_eq!(code_type_for_spec("evcxr"), Some(CodeType::Rust));
        assert_eq!(code_type_for_spec("unknown"), None);
    }

    #[test]
    fn go_round_trips_through_kernelspec_maps() {
        let spec_name = kernelspec_for(CodeType::Go);

        assert_eq!(spec_name, "gonb");
        assert_eq!(code_type_for_spec(spec_name), Some(CodeType::Go));
        assert_eq!(serde_json::to_value(CodeType::Go).unwrap(), "go");
    }

    #[test]
    fn jute_deck_metadata_survives_round_trip() {
        let json = r##"
            {
                "metadata": {
                    "jute_deck": {
                        "theme": "minimal-dark",
                        "aspect": "16:9",
                        "title": "Quarterly Results",
                        "author": "Analyst"
                    }
                },
                "nbformat_minor": 5,
                "nbformat": 4,
                "cells": [
                    {
                        "cell_type": "markdown",
                        "id": "550e8400-e29b-41d4-a716-446655440000",
                        "metadata": {
                            "jute_deck": {
                                "layout": "two-col",
                                "hidden": true,
                                "speaker_notes": "Pause here.",
                                "theme_override": "spur-brand",
                                "fragments": true,
                                "background": "#101010"
                            }
                        },
                        "source": "# Revenue"
                    }
                ]
            }
        "##;

        let notebook: NotebookRoot = serde_json::from_str(json).unwrap();
        let deck = notebook.metadata.jute_deck.as_ref().unwrap();
        assert_eq!(deck.theme.as_deref(), Some("minimal-dark"));
        assert_eq!(deck.aspect.as_deref(), Some("16:9"));
        assert_eq!(deck.title.as_deref(), Some("Quarterly Results"));
        assert_eq!(deck.author.as_deref(), Some("Analyst"));

        let Cell::Markdown(cell) = &notebook.cells[0] else {
            panic!("expected a markdown cell");
        };
        let cell_deck = cell.metadata.jute_deck.as_ref().unwrap();
        assert_eq!(cell_deck.layout, Some(JuteDeckLayout::TwoCol));
        assert_eq!(cell_deck.hidden, Some(true));
        assert_eq!(cell_deck.speaker_notes.as_deref(), Some("Pause here."));
        assert_eq!(cell_deck.theme_override.as_deref(), Some("spur-brand"));
        assert_eq!(cell_deck.fragments, Some(true));
        assert_eq!(cell_deck.background.as_deref(), Some("#101010"));

        let serialized = serde_json::to_value(&notebook).unwrap();
        assert_eq!(serialized["metadata"]["jute_deck"]["theme"], "minimal-dark");
        assert_eq!(
            serialized["cells"][0]["metadata"]["jute_deck"]["layout"],
            "two-col"
        );
        assert_eq!(
            serialized["cells"][0]["metadata"]["jute_deck"]["speaker_notes"],
            "Pause here."
        );
    }

    /// Regression test: None fields in NotebookMetadata must NOT serialize as
    /// `null` — the nbformat v4 JSON schema forbids `"kernelspec": null` etc.
    /// and `nbformat.read()` in Python crashes on such files.
    #[test]
    fn notebook_metadata_none_fields_omitted_not_null() {
        use serde_json::Map;

        // 1. Serializing a NotebookRoot whose five optional metadata fields are
        //    all None must produce a `metadata` object that does NOT contain
        //    the keys kernelspec, language_info, orig_nbformat, title, authors.
        let root = NotebookRoot {
            metadata: NotebookMetadata {
                kernelspec: None,
                language_info: None,
                orig_nbformat: None,
                title: None,
                authors: None,
                jute_deck: None,
                other: Map::new(),
            },
            nbformat_minor: 5,
            nbformat: 4,
            cells: Vec::new(),
        };
        let v = serde_json::to_value(&root).unwrap();
        let meta = v["metadata"].as_object().unwrap();
        assert!(
            !meta.contains_key("kernelspec"),
            "metadata must not contain kernelspec key when None, got: {meta:?}"
        );
        assert!(
            !meta.contains_key("language_info"),
            "metadata must not contain language_info key when None, got: {meta:?}"
        );
        assert!(
            !meta.contains_key("orig_nbformat"),
            "metadata must not contain orig_nbformat key when None, got: {meta:?}"
        );
        assert!(
            !meta.contains_key("title"),
            "metadata must not contain title key when None, got: {meta:?}"
        );
        assert!(
            !meta.contains_key("authors"),
            "metadata must not contain authors key when None, got: {meta:?}"
        );

        // 2. Deserializing a notebook with an empty `metadata` object must
        //    succeed (missing optional keys are OK).
        let json_empty_meta =
            r#"{"metadata": {}, "nbformat": 4, "nbformat_minor": 5, "cells": []}"#;
        let parsed: NotebookRoot = serde_json::from_str(json_empty_meta)
            .expect("empty metadata must deserialize without error");
        assert!(parsed.metadata.kernelspec.is_none());
        assert!(parsed.metadata.language_info.is_none());
        assert!(parsed.metadata.orig_nbformat.is_none());
        assert!(parsed.metadata.title.is_none());
        assert!(parsed.metadata.authors.is_none());

        // 3. Deserializing the legacy shape with explicit nulls must also
        //    succeed (backward compat with ~597 existing notebooks).
        let json_explicit_nulls = r#"{
            "metadata": {
                "kernelspec": null,
                "language_info": null,
                "orig_nbformat": null,
                "title": null,
                "authors": null
            },
            "nbformat": 4,
            "nbformat_minor": 5,
            "cells": []
        }"#;
        let parsed_legacy: NotebookRoot = serde_json::from_str(json_explicit_nulls)
            .expect("explicit-null metadata must deserialize without error (backward compat)");
        assert!(parsed_legacy.metadata.kernelspec.is_none());
        assert!(parsed_legacy.metadata.language_info.is_none());
        assert!(parsed_legacy.metadata.orig_nbformat.is_none());
        assert!(parsed_legacy.metadata.title.is_none());
        assert!(parsed_legacy.metadata.authors.is_none());

        // 3b. Self-heal: re-serializing a notebook parsed from the legacy null
        //     shape must produce JSON with the five keys ABSENT (not null).
        let healed = serde_json::to_value(&parsed_legacy).expect("re-serialize legacy notebook");
        let healed_meta = healed["metadata"].as_object().expect("metadata object");
        for key in [
            "kernelspec",
            "language_info",
            "orig_nbformat",
            "title",
            "authors",
        ] {
            assert!(
                !healed_meta.contains_key(key),
                "self-heal: {key} must be absent after round-trip, got: {healed_meta:?}"
            );
        }
    }

    #[test]
    fn string_to_multiline() {
        let empty = MultilineString::Single("".into()).normalize();
        assert_eq!(empty, MultilineString::Multi(vec![]));

        let single = MultilineString::Single("Hello, world!".into()).normalize();
        assert_eq!(
            single,
            MultilineString::Multi(vec!["Hello, world!".to_string()])
        );

        let multi = MultilineString::Single("Hello,\nworld!".into()).normalize();
        assert_eq!(
            multi,
            MultilineString::Multi(vec!["Hello,\n".to_string(), "world!".to_string()])
        );

        let multi = MultilineString::Single("Hello,\n\nworld!\n".into()).normalize();
        assert_eq!(
            multi,
            MultilineString::Multi(vec![
                "Hello,\n".to_string(),
                "\n".to_string(),
                "world!\n".to_string()
            ])
        );
    }
}
