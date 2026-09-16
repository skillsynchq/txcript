//! `txcript export` — write a session as a Simple interchange document.
//!
//! The source is resolved exactly like `view` (id, exact title, or an existing
//! Simple document file or stdin `-`, with optional `#range`). The output is the
//! full-fidelity Simple rendering of the canonical model — every
//! `Transcript<Common>` field has a slot in Simple — so the document is the
//! session as txcript sees it, detached from any harness's store.
//! `txcript continue <file> --with <harness>` brings it back into a harness,
//! on this machine or another.

use std::path::Path;
use std::process::ExitCode;

use txcript::harness::simple::Simple;
use txcript::{Codec, Common, HarnessId, TextCodec, Transcript};

use crate::{fragment, view};

pub fn cmd_export(
    source: &str,
    from: Option<HarnessId>,
    out: Option<&Path>,
) -> Result<ExitCode, String> {
    let (common, request) = view::load_source(source, from)?;
    let common = match &request {
        Some(req) => fragment::sliced(&common, req)?,
        None => common,
    };
    let text = render(&common)?;
    match out {
        Some(path) => {
            std::fs::write(path, text).map_err(|e| format!("writing {}: {e}", path.display()))?;
        }
        // A failed write means the reader is gone (`txcript export … | head`):
        // finish quietly instead of panicking the way `print!` would.
        None => {
            let _ = std::io::Write::write_all(&mut std::io::stdout(), text.as_bytes());
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// The Simple document for `common`: the canonical model rendered through
/// the Simple codec, which keeps every field.
pub fn render(common: &Transcript<Common>) -> Result<String, String> {
    let native = Simple::from_common(common).map_err(|e| e.to_string())?;
    Simple::to_text(&native).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use txcript::common::{
        Artifact, ArtifactSource, Block, ImageSource, Message, Meta, Role, StopReason, Tool,
        ToolOutput, Usage,
    };
    use txcript::harness::simple::Simple;
    use txcript::{Codec, TextCodec, Transcript};

    /// A transcript with every canonical field populated.
    #[allow(clippy::too_many_lines)]
    fn full() -> Transcript<txcript::Common> {
        let t0 = Utc.with_ymd_and_hms(2026, 8, 21, 9, 30, 0).unwrap();
        let meta = Meta {
            id: "sess-1".into(),
            timestamp: t0,
            cwd: Some("/work/repo".into()),
            git_branch: Some("main".into()),
            title: Some("Export round trip".into()),
            cli_version: Some("1.2.3".into()),
            model: Some("claude-fable-5".into()),
        };
        let messages = vec![
            Message {
                role: Role::User,
                content: vec![
                    Block::Text {
                        text: "run the tests".into(),
                    },
                    Block::Image {
                        source: ImageSource {
                            source_type: "base64".into(),
                            media_type: "image/png".into(),
                            data: "aGk=".into(),
                        },
                    },
                ],
                timestamp: t0,
                model: None,
                stop_reason: None,
                usage: None,
            },
            Message {
                role: Role::Assistant,
                content: vec![
                    Block::Thinking {
                        text: "cargo test covers it".into(),
                        signature: Some("sig".into()),
                        encrypted: Some("enc".into()),
                    },
                    Block::ToolUse {
                        id: "tu-1".into(),
                        tool: Tool::Bash {
                            command: "cargo test".into(),
                            workdir: Some("/work/repo".into()),
                            timeout_ms: Some(1000),
                            description: Some("tests".into()),
                            run_in_background: true,
                        },
                    },
                    Block::ToolUse {
                        id: "tu-2".into(),
                        tool: Tool::Raw {
                            tool_name: "mcp__x__y".into(),
                            input: serde_json::json!({"k": [1, 2]}),
                        },
                    },
                ],
                timestamp: t0,
                model: Some("claude-fable-5".into()),
                stop_reason: Some(StopReason::ToolUse),
                usage: Some(Usage {
                    input_tokens: 10,
                    output_tokens: 20,
                    cache_read_input_tokens: Some(5),
                    cache_creation_input_tokens: None,
                }),
            },
            Message {
                role: Role::User,
                content: vec![
                    Block::ToolResult {
                        tool_use_id: "tu-1".into(),
                        content: ToolOutput::Text("42 passed".into()),
                        is_error: false,
                    },
                    Block::ToolResult {
                        tool_use_id: "tu-2".into(),
                        content: ToolOutput::Json(serde_json::json!({"ok": false})),
                        is_error: true,
                    },
                ],
                timestamp: t0,
                model: None,
                stop_reason: None,
                usage: None,
            },
            Message {
                role: Role::Assistant,
                content: vec![
                    Block::Text {
                        text: "All green.".into(),
                    },
                    Block::Artifact {
                        artifact: Artifact {
                            id: "art-1".into(),
                            name: "report.md".into(),
                            source: ArtifactSource::Text {
                                text: "# done".into(),
                                media_type: Some("text/markdown".into()),
                            },
                        },
                    },
                ],
                timestamp: t0,
                model: None,
                stop_reason: Some(StopReason::Other("custom".into())),
                usage: None,
            },
        ];
        Transcript::new(meta, messages)
    }

    #[test]
    fn export_round_trips_every_canonical_field() {
        let original = full();
        let text = super::render(&original).unwrap();
        let parsed = Simple::from_text(&text).unwrap();
        let back = Simple::to_common(&parsed).unwrap();
        assert_eq!(back, original);
    }

    #[test]
    fn cmd_export_slices_existing_simple_document() {
        let dir = tempfile::tempdir().unwrap();
        let input_path = dir.path().join("input.json");
        let output_path = dir.path().join("sliced.json");
        std::fs::write(
            &input_path,
            r#"{
                "id": "doc-export-test",
                "messages": [
                    {"role": "user", "content": "msg 1"},
                    {"role": "assistant", "content": "msg 2"},
                    {"role": "user", "content": "msg 3"}
                ]
            }"#,
        )
        .unwrap();

        let source = format!("{}#1-2", input_path.display());
        let status = super::cmd_export(&source, None, Some(&output_path)).unwrap();
        assert_eq!(status, std::process::ExitCode::SUCCESS);

        let out_text = std::fs::read_to_string(&output_path).unwrap();
        let parsed = Simple::from_text(&out_text).unwrap();
        let common = Simple::to_common(&parsed).unwrap();
        assert_eq!(common.body.len(), 2);
    }
}
