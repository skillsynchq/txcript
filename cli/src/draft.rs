//! A message as an editable text file, and the way back.
//!
//! The crop editor hands one message to the user's editor as plain text.
//! A message with one editable block is that block's text and nothing
//! else. One with several is one `▸` section per block, labelled the way
//! the viewer labels them. Text and thinking are edited as they are; a
//! tool's input is edited as JSON; a tool result as its text (or JSON).
//! Images and artifacts are not in the file and come back untouched.
//!
//! Sections are matched back to blocks in order, by their heading lines, so
//! the number and order of headings must survive the edit. An emptied text
//! or thinking section drops that block; a tool call or result is never
//! dropped, so a call always keeps its result.

use txcript::common::{Block, Message, Tool, ToolOutput};

/// The message as the file the editor opens, or `None` when nothing in it
/// can be edited.
#[must_use]
pub(crate) fn draft(message: &Message) -> Option<String> {
    let sections: Vec<(String, String)> = message
        .content
        .iter()
        .filter_map(|block| Some((heading(block)?, body(block))))
        .collect();
    let mut out = String::new();
    match sections.as_slice() {
        [] => return None,
        [(_, body)] => {
            out.push_str(body);
            if !body.ends_with('\n') {
                out.push('\n');
            }
        }
        sections => {
            for (index, (heading, body)) in sections.iter().enumerate() {
                if index > 0 {
                    out.push('\n');
                }
                out.push_str("▸ ");
                out.push_str(heading);
                out.push('\n');
                out.push_str(body);
                if !body.ends_with('\n') {
                    out.push('\n');
                }
            }
        }
    }
    Some(out)
}

/// The message with the edits in `text` applied.
///
/// # Errors
/// When a heading is missing, a tool input or JSON result no longer
/// parses, or a tool call or result was emptied.
pub(crate) fn apply(message: &Message, text: &str) -> Result<Message, String> {
    let editable: Vec<(usize, String)> = message
        .content
        .iter()
        .enumerate()
        .filter_map(|(index, block)| Some((index, heading(block)?)))
        .collect();
    let sections = if editable.len() == 1 {
        vec![text.trim_end_matches('\n').to_string()]
    } else {
        split(text, &editable)?
    };
    let mut content = Vec::with_capacity(message.content.len());
    let mut edited = sections.into_iter();
    for block in &message.content {
        if heading(block).is_none() {
            content.push(block.clone());
            continue;
        }
        let section = edited.next().unwrap_or_default();
        if let Some(block) = rebuild(block, &section)? {
            content.push(block);
        }
    }
    Ok(Message {
        content,
        ..message.clone()
    })
}

/// The heading a block is edited under, or `None` when it is not editable.
fn heading(block: &Block) -> Option<String> {
    match block {
        Block::Text { .. } => Some("Text".to_string()),
        Block::Thinking { .. } => Some("Thinking".to_string()),
        Block::ToolUse { tool, .. } => Some(format!("Tool · {}", tool.to_canonical().0)),
        Block::ToolResult { .. } => Some("Result".to_string()),
        Block::Image { .. } | Block::Artifact { .. } => None,
    }
}

fn body(block: &Block) -> String {
    match block {
        Block::Text { text } | Block::Thinking { text, .. } => text.clone(),
        Block::ToolUse { tool, .. } => pretty(&tool.to_canonical().1),
        Block::ToolResult { content, .. } => match content {
            ToolOutput::Text(text) => text.clone(),
            ToolOutput::Json(value) => pretty(value),
        },
        Block::Image { .. } | Block::Artifact { .. } => String::new(),
    }
}

fn pretty(value: &serde_json::Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

/// The text under each expected heading, in order. Anything before the
/// first heading is ignored.
fn split(text: &str, headings: &[(usize, String)]) -> Result<Vec<String>, String> {
    let mut sections = Vec::with_capacity(headings.len());
    let mut lines = text.lines().peekable();
    let mut current: Option<String> = None;
    let mut expected = headings.iter().map(|(_, heading)| heading.as_str());
    let mut next = expected.next();
    while let Some(line) = lines.next() {
        if let Some(heading) = next
            && line.strip_prefix("▸ ").is_some_and(|rest| rest == heading)
        {
            if let Some(done) = current.replace(String::new()) {
                sections.push(done);
            }
            next = expected.next();
            continue;
        }
        if let Some(section) = &mut current {
            section.push_str(line);
            if lines.peek().is_some() {
                section.push('\n');
            }
        }
    }
    if let Some(done) = current {
        sections.push(done);
    }
    if let Some(missing) = next {
        return Err(format!(
            "the `▸ {missing}` heading is missing from the file"
        ));
    }
    Ok(sections
        .into_iter()
        .map(|section| section.trim_end_matches('\n').to_string())
        .collect())
}

/// `block` with `section` as its new body; `None` drops the block.
fn rebuild(block: &Block, section: &str) -> Result<Option<Block>, String> {
    Ok(match block {
        Block::Text { text } => {
            if section.is_empty() {
                None
            } else if section == text.trim_end_matches('\n') {
                Some(block.clone())
            } else {
                Some(Block::Text {
                    text: section.to_string(),
                })
            }
        }
        Block::Thinking { text, .. } => {
            if section.is_empty() {
                None
            } else if section == text.trim_end_matches('\n') {
                Some(block.clone())
            } else {
                // The provider's signature covers the original text only.
                Some(Block::Thinking {
                    text: section.to_string(),
                    signature: None,
                    encrypted: None,
                })
            }
        }
        Block::ToolUse { id, tool } => {
            if section.trim().is_empty() {
                return Err("a tool call cannot be emptied; remove the message instead".into());
            }
            let (name, input) = tool.to_canonical();
            let edited: serde_json::Value = serde_json::from_str(section)
                .map_err(|error| format!("the input of `{name}` is not valid JSON: {error}"))?;
            if edited == input {
                Some(block.clone())
            } else {
                Some(Block::ToolUse {
                    id: id.clone(),
                    tool: Tool::from_canonical(&name, edited),
                })
            }
        }
        Block::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => {
            let edited = match content {
                ToolOutput::Text(text) if section == text.trim_end_matches('\n') => {
                    return Ok(Some(block.clone()));
                }
                ToolOutput::Text(_) => ToolOutput::Text(section.to_string()),
                ToolOutput::Json(value) => {
                    let edited: serde_json::Value = serde_json::from_str(section)
                        .map_err(|error| format!("the tool result is not valid JSON: {error}"))?;
                    if edited == *value {
                        return Ok(Some(block.clone()));
                    }
                    ToolOutput::Json(edited)
                }
            };
            Some(Block::ToolResult {
                tool_use_id: tool_use_id.clone(),
                content: edited,
                is_error: *is_error,
            })
        }
        Block::Image { .. } | Block::Artifact { .. } => Some(block.clone()),
    })
}

#[cfg(test)]
mod tests {
    use txcript::common::{ImageSource, Role};

    use super::*;

    fn image() -> Block {
        Block::Image {
            source: ImageSource {
                source_type: "base64".into(),
                media_type: "image/png".into(),
                data: "AA==".into(),
            },
        }
    }

    fn message(role: Role, content: Vec<Block>) -> Message {
        Message {
            role,
            content,
            timestamp: chrono::DateTime::UNIX_EPOCH,
            model: None,
            stop_reason: None,
            usage: None,
        }
    }

    fn text(text: &str) -> Block {
        Block::Text { text: text.into() }
    }

    #[test]
    fn a_draft_lists_each_editable_block_under_its_heading() {
        let message = message(
            Role::Assistant,
            vec![
                Block::Thinking {
                    text: "hmm".into(),
                    signature: Some("sig".into()),
                    encrypted: None,
                },
                text("hello\n"),
                Block::ToolUse {
                    id: "t1".into(),
                    tool: Tool::from_canonical("Bash", serde_json::json!({"command": "ls"})),
                },
                image(),
            ],
        );
        let draft = draft(&message).unwrap();
        assert_eq!(
            draft,
            "▸ Thinking\nhmm\n\n▸ Text\nhello\n\n▸ Tool · Bash\n{\n  \"command\": \"ls\"\n}\n"
        );
        assert_eq!(
            super::draft(&super::tests::message(Role::User, vec![image()])),
            None
        );
    }

    #[test]
    fn a_message_with_one_editable_block_is_just_its_text() {
        let message = message(Role::User, vec![text("hello\nworld"), image()]);
        assert_eq!(draft(&message).unwrap(), "hello\nworld\n");
        let edited = apply(&message, "hello there\n").unwrap();
        assert_eq!(edited.content[0], text("hello there"));
        assert_eq!(edited.content[1], image());
        // Text that looks like a heading is still just text.
        let edited = apply(&message, "▸ Text\nnot a heading\n").unwrap();
        assert_eq!(edited.content[0], text("▸ Text\nnot a heading"));
        assert_eq!(apply(&message, "hello\nworld\n").unwrap(), message);
        assert_eq!(apply(&message, "\n").unwrap().content, vec![image()]);
    }

    #[test]
    fn an_unchanged_draft_gives_back_the_same_message() {
        let message = message(
            Role::User,
            vec![
                text("one\n"),
                Block::ToolResult {
                    tool_use_id: "t1".into(),
                    content: ToolOutput::Json(serde_json::json!({"ok": true})),
                    is_error: false,
                },
                image(),
            ],
        );
        let draft = draft(&message).unwrap();
        assert_eq!(apply(&message, &draft).unwrap(), message);
    }

    #[test]
    fn edits_land_in_their_blocks_and_thinking_loses_its_signature() {
        let message = message(
            Role::Assistant,
            vec![
                Block::Thinking {
                    text: "hmm".into(),
                    signature: Some("sig".into()),
                    encrypted: Some("enc".into()),
                },
                text("hello"),
                Block::ToolUse {
                    id: "t1".into(),
                    tool: Tool::from_canonical("Bash", serde_json::json!({"command": "ls"})),
                },
                text("bye"),
            ],
        );
        let edited = apply(
            &message,
            "▸ Thinking\nstill hmm\n▸ Text\nhello there\nsecond line\n\n▸ Tool · Bash\n{\"command\": \"ls -la\"}\n▸ Text\n",
        )
        .unwrap();
        assert_eq!(
            edited.content[0],
            Block::Thinking {
                text: "still hmm".into(),
                signature: None,
                encrypted: None
            }
        );
        assert_eq!(edited.content[1], text("hello there\nsecond line"));
        assert_eq!(
            edited.content[2].clone(),
            Block::ToolUse {
                id: "t1".into(),
                tool: Tool::from_canonical("Bash", serde_json::json!({"command": "ls -la"})),
            }
        );
        // The emptied trailing text block is gone.
        assert_eq!(edited.content.len(), 3);
        assert_eq!(edited.role, Role::Assistant);
    }

    #[test]
    fn a_tool_result_can_be_trimmed_but_not_dropped_and_json_must_parse() {
        let message = message(
            Role::User,
            vec![
                Block::ToolResult {
                    tool_use_id: "t1".into(),
                    content: ToolOutput::Text("a\nb\nc".into()),
                    is_error: true,
                },
                Block::ToolResult {
                    tool_use_id: "t2".into(),
                    content: ToolOutput::Json(serde_json::json!({"n": 1})),
                    is_error: false,
                },
            ],
        );
        let edited = apply(&message, "▸ Result\na\n▸ Result\n{\"n\": 2}\n").unwrap();
        assert_eq!(
            edited.content[0],
            Block::ToolResult {
                tool_use_id: "t1".into(),
                content: ToolOutput::Text("a".into()),
                is_error: true,
            }
        );
        assert_eq!(
            edited.content[1],
            Block::ToolResult {
                tool_use_id: "t2".into(),
                content: ToolOutput::Json(serde_json::json!({"n": 2})),
                is_error: false,
            }
        );
        let emptied = apply(&message, "▸ Result\n\n▸ Result\n{\"n\": 2}\n").unwrap();
        assert_eq!(emptied.content.len(), 2);
        assert_eq!(
            emptied.content[0],
            Block::ToolResult {
                tool_use_id: "t1".into(),
                content: ToolOutput::Text(String::new()),
                is_error: true,
            }
        );
        let error = apply(&message, "▸ Result\na\n▸ Result\nnot json\n").unwrap_err();
        assert!(error.contains("not valid JSON"), "{error}");
    }

    #[test]
    fn a_missing_heading_or_emptied_tool_call_is_refused() {
        let message = message(
            Role::Assistant,
            vec![
                text("hello"),
                Block::ToolUse {
                    id: "t1".into(),
                    tool: Tool::from_canonical("Bash", serde_json::json!({"command": "ls"})),
                },
            ],
        );
        let error = apply(&message, "▸ Text\nhello\n").unwrap_err();
        assert!(
            error.contains("`▸ Tool · Bash` heading is missing"),
            "{error}"
        );
        let error = apply(&message, "▸ Text\nhello\n▸ Tool · Bash\n\n").unwrap_err();
        assert!(error.contains("cannot be emptied"), "{error}");
        // A content line that merely resembles a heading is not one.
        let edited = apply(&message, "▸ Text\n▸ Textual\n▸ Tool · Bash\n{}\n").unwrap();
        assert_eq!(edited.content[0], text("▸ Textual"));
    }
}
