//! A compact, one-way text projection of the canonical transcript model.
//!
//! [`to_text`] is intended for LLM context, not archival or round trips. It
//! preserves conversational content and compact tool data while discarding
//! replay-only data: message timestamps, usage, stop reasons, reasoning
//! signatures/encrypted payloads, and inline image bytes.
//!
//! [`to_text_fragment`] renders a [`Span`] of the body in the same format,
//! with a `── #N ──` rule numbering each message by its 1-based position in
//! the full session — the same ordinals fragment refs (`<id>#5-12`) use, so
//! what a reader sees is what they can reference.

use std::collections::HashMap;
use std::fmt::Write as _;

use crate::common::{Block, Message, Meta, Role};
use crate::{Common, Span, Transcript};

/// Render a canonical transcript as compact, LLM-oriented text.
///
/// The format uses short bracketed labels instead of repeating the canonical
/// JSON schema. Tool-call ids are remapped to session-local integers so tool
/// results remain paired without carrying provider-generated identifiers.
#[must_use]
pub fn to_text(transcript: &Transcript<Common>) -> String {
    let mut out = String::new();
    header(&mut out, &transcript.meta);

    let mut tool_ids = HashMap::<&str, usize>::new();
    let mut next_tool_id = 1;
    for message in &transcript.body {
        blocks(&mut out, &mut tool_ids, &mut next_tool_id, message);
    }

    out
}

/// Render `span` of the transcript in [`to_text`]'s format, a `── #N ──`
/// rule before each message carrying its 1-based ordinal in the full body.
/// Partial spans add `fragment=`/`of=` header fields naming the slice.
/// `None` when `span` is out of bounds, mirroring [`Transcript::fragment`].
#[must_use]
pub fn to_text_fragment(transcript: &Transcript<Common>, span: &Span) -> Option<String> {
    transcript.fragment(span).map(|messages| {
        let mut out = String::new();
        header(&mut out, &transcript.meta);
        let total = transcript.body.len();
        if span.0 != (0..total) {
            field(&mut out, "fragment", &format_span(span));
            field(&mut out, "of", &total.to_string());
        }

        let mut tool_ids = HashMap::<&str, usize>::new();
        let mut next_tool_id = 1;
        for (offset, message) in messages.iter().enumerate() {
            // No trailing newline: `section` supplies the separator, keeping
            // the rule flush against the first label under it.
            let _ = write!(out, "\n── #{} ──", span.0.start + offset + 1);
            blocks(&mut out, &mut tool_ids, &mut next_tool_id, message);
        }
        out
    })
}

/// Human-facing `#a-b` (`#a` for a single message) for a resolved span.
fn format_span(span: &Span) -> String {
    match span.0.len() {
        1 => format!("#{}", span.0.start + 1),
        _ => format!("#{}-{}", span.0.start + 1, span.0.end),
    }
}

fn header(out: &mut String, meta: &Meta) {
    out.push_str("[session]\n");
    field(out, "id", &meta.id);
    field(out, "started", &meta.timestamp.to_rfc3339());
    optional_field(out, "title", meta.title.as_deref());
    optional_field(out, "cwd", meta.cwd.as_deref());
    optional_field(out, "branch", meta.git_branch.as_deref());
    optional_field(out, "model", meta.model.as_deref());
}

/// Render one message's blocks. The tool-id map is threaded across calls so
/// `[tool N …]`/`[result N]` stay paired over the whole render.
fn blocks<'a>(
    out: &mut String,
    tool_ids: &mut HashMap<&'a str, usize>,
    next_tool_id: &mut usize,
    message: &'a Message,
) {
    for block in &message.content {
        match block {
            Block::Text { text } => {
                let label = match message.role {
                    Role::User => "user",
                    Role::Assistant => "assistant",
                };
                section(out, label, text);
            }
            Block::Thinking { text, .. } => section(out, "thinking", text),
            Block::ToolUse { id, tool } => {
                let short_id = short_tool_id(tool_ids, next_tool_id, id);
                let (name, input) = tool.to_canonical();
                // A tool invoked with no arguments — a bare slash command,
                // say — renders as its label alone rather than a stray `{}`.
                let body = match &input {
                    serde_json::Value::Null => String::new(),
                    serde_json::Value::Object(map) if map.is_empty() => String::new(),
                    input => input.to_string(),
                };
                section(out, &format!("tool {short_id} {}", one_line(&name)), &body);
            }
            Block::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                let short_id = short_tool_id(tool_ids, next_tool_id, tool_use_id);
                let error = if *is_error { " error" } else { "" };
                let label = format!("result {short_id}{error}");
                section(out, &label, &content.as_text());
            }
            Block::Image { source } => section(
                out,
                &format!("image {} omitted", one_line(&source.media_type)),
                "",
            ),
            Block::Artifact { artifact } => {
                let location = match &artifact.source {
                    crate::common::ArtifactSource::Path { path, .. } => path.as_str(),
                    crate::common::ArtifactSource::Text { text, .. } => text.as_str(),
                    crate::common::ArtifactSource::Base64 { .. } => "binary data omitted",
                };
                section(
                    out,
                    &format!("artifact {}", one_line(&artifact.name)),
                    location,
                );
            }
        }
    }
}

fn field(out: &mut String, name: &str, value: &str) {
    let _ = writeln!(out, "{name}={}", one_line(value));
}

fn optional_field(out: &mut String, name: &str, value: Option<&str>) {
    if let Some(value) = value.filter(|value| !value.is_empty()) {
        field(out, name, value);
    }
}

fn one_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn section(out: &mut String, label: &str, text: &str) {
    if !out.ends_with("\n\n") {
        out.push('\n');
    }
    let _ = writeln!(out, "[{label}]");
    out.push_str(text);
    if !text.ends_with('\n') {
        out.push('\n');
    }
}

fn short_tool_id<'a>(
    ids: &mut HashMap<&'a str, usize>,
    next_id: &mut usize,
    provider_id: &'a str,
) -> usize {
    *ids.entry(provider_id).or_insert_with(|| {
        let id = *next_id;
        *next_id += 1;
        id
    })
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Utc};
    use serde_json::json;

    use super::*;
    use crate::common::{ImageSource, Message, Meta, StopReason, Tool, ToolOutput, Usage};

    fn transcript(body: Vec<Message>) -> Transcript<Common> {
        Transcript::new(
            Meta {
                id: "provider-session-id".into(),
                timestamp: DateTime::<Utc>::UNIX_EPOCH,
                cwd: Some("/work/project".into()),
                git_branch: Some("main".into()),
                title: Some("Fix the parser".into()),
                cli_version: Some("9.9.9".into()),
                model: Some("model-name".into()),
                lineage: None,
            },
            body,
        )
    }

    fn message(role: Role, content: Vec<Block>) -> Message {
        Message {
            role,
            content,
            timestamp: DateTime::<Utc>::UNIX_EPOCH,
            model: None,
            stop_reason: Some(StopReason::EndTurn),
            usage: Some(Usage {
                input_tokens: 100,
                output_tokens: 20,
                cache_read_input_tokens: None,
                cache_creation_input_tokens: None,
            }),
        }
    }

    #[test]
    fn renders_conversation_and_omits_replay_only_data() {
        let rendered = to_text(&transcript(vec![
            message(
                Role::User,
                vec![Block::Text {
                    text: "Please fix it.".into(),
                }],
            ),
            message(
                Role::Assistant,
                vec![
                    Block::Thinking {
                        text: "I should inspect the file.".into(),
                        signature: Some("large-signature".into()),
                        encrypted: Some("large-encrypted-payload".into()),
                    },
                    Block::Text {
                        text: "I found the issue.".into(),
                    },
                    Block::Image {
                        source: ImageSource {
                            source_type: "base64".into(),
                            media_type: "image/png".into(),
                            data: "large-base64-payload".into(),
                        },
                    },
                ],
            ),
        ]));

        assert!(rendered.contains("[user]\nPlease fix it."));
        assert!(rendered.contains("[thinking]\nI should inspect the file."));
        assert!(rendered.contains("[assistant]\nI found the issue."));
        assert!(rendered.contains("[image image/png omitted]"));
        assert!(!rendered.contains("large-signature"));
        assert!(!rendered.contains("large-encrypted-payload"));
        assert!(!rendered.contains("large-base64-payload"));
        assert!(!rendered.contains("input_tokens"));
        assert!(!rendered.contains("cli_version"));
    }

    #[test]
    fn compacts_tool_json_and_shortens_provider_ids() {
        let rendered = to_text(&transcript(vec![
            message(
                Role::Assistant,
                vec![Block::ToolUse {
                    id: "provider-generated-tool-id-with-many-tokens".into(),
                    tool: Tool::Read {
                        file_path: "src/lib.rs".into(),
                        offset: None,
                        limit: Some(20),
                    },
                }],
            ),
            message(
                Role::User,
                vec![Block::ToolResult {
                    tool_use_id: "provider-generated-tool-id-with-many-tokens".into(),
                    content: ToolOutput::Json(json!({"lines": ["one", "two"]})),
                    is_error: false,
                }],
            ),
        ]));

        assert!(rendered.contains("[tool 1 Read]\n{\"file_path\":\"src/lib.rs\",\"limit\":20}"));
        assert!(rendered.contains("[result 1]\n{\"lines\":[\"one\",\"two\"]}"));
        assert!(!rendered.contains("provider-generated-tool-id-with-many-tokens"));
    }

    #[test]
    fn unmatched_results_still_receive_stable_short_ids() {
        let rendered = to_text(&transcript(vec![message(
            Role::User,
            vec![
                Block::ToolResult {
                    tool_use_id: "second".into(),
                    content: ToolOutput::Text("failed".into()),
                    is_error: true,
                },
                Block::ToolResult {
                    tool_use_id: "second".into(),
                    content: ToolOutput::Text("again".into()),
                    is_error: false,
                },
            ],
        )]));

        assert!(rendered.contains("[result 1 error]\nfailed"));
        assert!(rendered.contains("[result 1]\nagain"));
    }

    fn three_messages() -> Transcript<Common> {
        transcript(vec![
            message(
                Role::User,
                vec![Block::Text {
                    text: "first".into(),
                }],
            ),
            message(
                Role::Assistant,
                vec![Block::Text {
                    text: "second".into(),
                }],
            ),
            message(
                Role::User,
                vec![Block::Text {
                    text: "third".into(),
                }],
            ),
        ])
    }

    #[test]
    fn fragment_numbers_messages_with_full_session_ordinals() {
        let full = to_text_fragment(&three_messages(), &Span(0..3)).unwrap();
        assert!(full.contains("── #1 ──\n[user]\nfirst"));
        assert!(full.contains("── #3 ──\n[user]\nthird"));
        assert!(!full.contains("fragment="));

        let partial = to_text_fragment(&three_messages(), &Span(1..3)).unwrap();
        assert!(partial.contains("fragment=#2-3"));
        assert!(partial.contains("of=3"));
        assert!(partial.contains("── #2 ──\n[assistant]\nsecond"));
        assert!(!partial.contains("first"));

        let single = to_text_fragment(&three_messages(), &Span(1..2)).unwrap();
        assert!(single.contains("fragment=#2\n"));

        assert!(to_text_fragment(&three_messages(), &Span(1..4)).is_none());
    }

    #[test]
    fn fragment_tool_ids_stay_paired_across_messages() {
        let rendered = to_text_fragment(
            &transcript(vec![
                message(
                    Role::Assistant,
                    vec![Block::ToolUse {
                        id: "provider-a".into(),
                        tool: Tool::Raw {
                            tool_name: "Bash".into(),
                            input: json!({"command": "ls"}),
                        },
                    }],
                ),
                message(
                    Role::User,
                    vec![Block::ToolResult {
                        tool_use_id: "provider-a".into(),
                        content: ToolOutput::Text("ok".into()),
                        is_error: false,
                    }],
                ),
            ]),
            &Span(0..2),
        )
        .unwrap();

        assert!(rendered.contains("[tool 1 Bash]"));
        assert!(rendered.contains("[result 1]\nok"));
        assert!(!rendered.contains("provider-a"));
    }
}
