//! Read-only MCP tools over local coding-agent sessions.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::{Implementation, JsonObject, ServerCapabilities, ServerInfo};
use rmcp::schemars::JsonSchema;
use rmcp::{
    ErrorData, ServerHandler, ServiceExt, tool, tool_handler, tool_router, transport::stdio,
};
use serde::{Deserialize, Serialize};
use txcript::common::Meta;
use txcript::search::{DocMatch, Hit, Origin};
use txcript::{HarnessId, Span, local, text};

/// Ceiling on one `read_session` response, in rendered bytes. Reads over it
/// are refused with ready-made `#range` chunks so a caller never floods its
/// own context by accident; an explicitly requested single message is always
/// served, however large.
const READ_BUDGET: usize = 100_000;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListSessionsRequest {
    /// Only include this harness. Omit to include every harness.
    from: Option<String>,
    /// Only include sessions recorded in or under this working directory.
    /// Omit to include every directory. Sessions without a recorded cwd are
    /// excluded when this filter is present.
    cwd: Option<String>,
    /// Return at most this many sessions. Omit for no cap.
    limit: Option<usize>,
    /// Skip this many sessions from the newest end first — page with
    /// `limit`. Omit for 0.
    offset: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SearchSessionsRequest {
    /// Text to find. Matched literally and case-insensitively: it must
    /// appear in a line exactly as written, spaces included.
    pattern: String,
    /// Search only this harness. Omit to search every harness.
    from: Option<String>,
    /// Search only sessions recorded in or under this working directory.
    /// Omit to search every directory. Sessions without a recorded cwd are
    /// excluded when this filter is present.
    cwd: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReadSessionRequest {
    /// Session id (any unambiguous prefix works) or exact title, with an
    /// optional `#range` of 1-based inclusive message numbers (`abc#5-12`,
    /// `abc#7`, `abc#5-`, `abc#-10`). Reads rendering past the byte budget
    /// are refused with suggested sub-ranges to request instead.
    id: String,
    /// Only look in this harness. Omit to look across every harness.
    from: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct SessionList {
    /// Sessions matching the filters, before `limit`/`offset` paging.
    total: usize,
    /// Index of the first returned session within that filtered set.
    offset: usize,
    sessions: Vec<SessionSummary>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct SessionSummary {
    harness: String,
    id: String,
    timestamp: String,
    title: Option<String>,
    cwd: Option<String>,
    git_branch: Option<String>,
    model: Option<String>,
}

impl SessionSummary {
    fn new(harness: HarnessId, meta: &Meta) -> Self {
        Self {
            harness: harness.to_string(),
            id: meta.id.clone(),
            timestamp: meta.timestamp.to_rfc3339(),
            title: meta.title.clone(),
            cwd: meta.cwd.clone(),
            git_branch: meta.git_branch.clone(),
            model: meta.model.clone(),
        }
    }
}

#[derive(Debug, Serialize, JsonSchema)]
struct SearchResults {
    matches: Vec<SearchMatch>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct SearchMatch {
    session: SessionSummary,
    score: u32,
    hits: Vec<SearchHit>,
}

impl SearchMatch {
    fn new(found: &DocMatch<'_>) -> Self {
        Self {
            session: SessionSummary::new(found.key.harness, found.meta),
            score: found.score,
            hits: found.hits.iter().map(SearchHit::from).collect(),
        }
    }
}

#[derive(Debug, Serialize, JsonSchema)]
struct SearchHit {
    /// Half-open message range the hit resolves to; empty for meta hits,
    /// which match the session header rather than a message.
    span: std::ops::Range<usize>,
    block: usize,
    origin: &'static str,
    line: String,
    score: u32,
}

impl From<&Hit> for SearchHit {
    fn from(hit: &Hit) -> Self {
        Self {
            span: hit.span.0.clone(),
            block: hit.block,
            origin: origin_name(hit.origin),
            line: hit.line.clone(),
            score: hit.score,
        }
    }
}

fn origin_name(origin: Origin) -> &'static str {
    match origin {
        Origin::User => "user",
        Origin::Assistant => "assistant",
        Origin::Thinking => "thinking",
        Origin::ToolUse => "tool_use",
        Origin::ToolResult => "tool_result",
        Origin::Meta => "meta",
    }
}

/// The read-only session MCP server used by `txcript mcp`.
///
/// The type also implements [`ServerHandler`], so a larger MCP server can
/// delegate session tool listing and calls to it.
#[derive(Clone)]
pub struct SessionServer {
    tool_router: ToolRouter<Self>,
    /// Persistent search cache for `search_sessions`; `None` rebuilds the
    /// index from scratch on every call.
    cache: Option<PathBuf>,
}

impl SessionServer {
    /// Create the session server. `cache` has the same semantics as the CLI's
    /// global `--cache`: `None` rebuilds the search index on each call.
    #[must_use]
    pub fn new(cache: Option<PathBuf>) -> Self {
        let mut tool_router = Self::tool_router();
        for route in tool_router.map.values_mut() {
            strip_nonstandard_formats(Arc::make_mut(&mut route.attr.input_schema));
            if let Some(output) = route.attr.output_schema.as_mut() {
                strip_nonstandard_formats(Arc::make_mut(output));
            }
        }
        Self { tool_router, cache }
    }
}

/// The `format` values JSON Schema 2020-12 defines.
const STANDARD_FORMATS: &[&str] = &[
    "date",
    "date-time",
    "duration",
    "email",
    "hostname",
    "idn-email",
    "idn-hostname",
    "ipv4",
    "ipv6",
    "iri",
    "iri-reference",
    "json-pointer",
    "regex",
    "relative-json-pointer",
    "time",
    "uri",
    "uri-reference",
    "uri-template",
    "uuid",
];

/// Drop `format` annotations JSON Schema does not define, in place.
///
/// `schemars` labels Rust integer widths with OpenAPI-style formats — `uint`
/// for `usize`, `uint32` for `u32` — which are not JSON Schema formats, so a
/// strict client logs a warning for every occurrence. Removing them leaves
/// `type` and `minimum`, which state the same constraint in a form every
/// validator understands.
///
/// This runs over the whole generated document rather than through a
/// `JsonSchema` derive attribute, because the widths also appear under
/// `$defs` — `Range_of_uint` for a `std::ops::Range<usize>` field — which a
/// per-type transform never sees.
fn strip_nonstandard_formats(schema: &mut JsonObject) {
    if schema
        .get("format")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|format| !STANDARD_FORMATS.contains(&format))
    {
        schema.remove("format");
    }
    for value in schema.values_mut() {
        strip_nested_formats(value);
    }
}

fn strip_nested_formats(value: &mut serde_json::Value) {
    match value {
        // A property literally named `format` holds a schema, not a string,
        // so the `as_str` guard above leaves it alone.
        serde_json::Value::Object(object) => strip_nonstandard_formats(object),
        serde_json::Value::Array(items) => items.iter_mut().for_each(strip_nested_formats),
        _ => {}
    }
}

#[tool_router]
#[allow(
    clippy::unused_self,
    reason = "rmcp routes tools through methods on the server instance"
)]
impl SessionServer {
    /// List local sessions newest-first, with the same harness and working
    /// directory filters as `txcript list`, paged by `limit`/`offset`.
    #[tool(
        description = "List local coding-agent sessions newest-first. Optional `from` and `cwd` filters match the txcript CLI; omitted filters include all harnesses or directories. Live web sources are not listed here. Optional `limit`/`offset` page the listing; the result carries the pre-paging `total`.",
        annotations(title = "List sessions", read_only_hint = true)
    )]
    fn list_sessions(
        &self,
        Parameters(request): Parameters<ListSessionsRequest>,
    ) -> Result<Json<SessionList>, ErrorData> {
        let from = parse_from(request.from.as_deref())?;
        // Enumerating a live web account is not offered over MCP; the
        // refusal is explicit so an agent doesn't read "no sessions" as truth.
        if matches!(from, Some(HarnessId::ClaudeChat | HarnessId::ChatGpt)) {
            let name = from.map_or("live source", HarnessId::as_str);
            return Err(ErrorData::invalid_params(
                format!(
                    "list_sessions does not enumerate {name}; use `txcript list --from {name}`"
                ),
                None,
            ));
        }
        let cwd = request.cwd.as_deref().map(Path::new);
        let all: Vec<SessionSummary> = local::discover()
            .into_iter()
            .filter(|session| super::selected(session, from, cwd))
            .map(|session| SessionSummary::new(session.harness, &session.meta))
            .collect();
        let total = all.len();
        let offset = request.offset.unwrap_or(0).min(total);
        let sessions = all
            .into_iter()
            .skip(offset)
            .take(request.limit.unwrap_or(usize::MAX))
            .collect();
        Ok(Json(SessionList {
            total,
            offset,
            sessions,
        }))
    }

    /// Search local session content with the same matching, harness, and
    /// working-directory behavior as `txcript query <pattern>`.
    #[tool(
        description = "Search local coding-agent sessions for a literal, case-insensitive pattern: it must appear in a line exactly as written, spaces included. Optional `from` and `cwd` filters match the txcript CLI; omitted filters search all harnesses or directories.",
        annotations(title = "Search sessions", read_only_hint = true)
    )]
    fn search_sessions(
        &self,
        Parameters(request): Parameters<SearchSessionsRequest>,
    ) -> Result<Json<SearchResults>, ErrorData> {
        let from = parse_from(request.from.as_deref())?;
        let cwd = request.cwd.as_deref().map(Path::new);
        let index = super::query::index_for(from, cwd, self.cache.as_deref())
            .map_err(|error| ErrorData::internal_error(error, None))?;
        let mut query = super::query::user_query(&request.pattern);
        // Match the CLI's one-shot output bounds.
        query.limit = Some(20);
        query.hits_per_doc = Some(3);
        let matches = index.query(&query).iter().map(SearchMatch::new).collect();
        Ok(Json(SearchResults { matches }))
    }

    /// Read one session — or a `#range` of its messages — as the
    /// token-optimized text projection `txcript view` prints. The optional
    /// harness scope behaves like `--from` on `txcript continue`.
    #[tool(
        description = "Read a local session by id (any unambiguous prefix) or exact title as token-optimized text. Append `#range` (1-based inclusive, e.g. `abc#5-12`) to read part of it; reads over the byte budget are refused with suggested sub-ranges. Omit `from` to search every harness.",
        annotations(title = "Read session", read_only_hint = true)
    )]
    fn read_session(
        &self,
        Parameters(request): Parameters<ReadSessionRequest>,
    ) -> Result<String, ErrorData> {
        let from = parse_from(request.from.as_deref())?;
        if let Some(loaded) = super::load_direct_claude_chat(&request.id, from) {
            let (common, span_req) =
                loaded.map_err(|error| ErrorData::internal_error(error, None))?;
            let src = crate::fragment::parse_ref(&request.id).0;
            return render_read_session(src, &common, span_req.as_ref());
        }
        if let Some(loaded) = super::load_direct_chatgpt(&request.id, from) {
            let (common, span_req) =
                loaded.map_err(|error| ErrorData::internal_error(error, None))?;
            let src = crate::fragment::parse_ref(&request.id).0;
            return render_read_session(src, &common, span_req.as_ref());
        }
        let sessions = discover_scoped(from)?;
        // A whole-input match (a title that itself contains `#12`) beats the
        // fragment interpretation, as everywhere else.
        let (src, span_req) = match crate::fragment::parse_ref(&request.id) {
            (_, Some(_)) if super::find_exact(&sessions, from, &request.id).is_some() => {
                (request.id.as_str(), None)
            }
            parsed => parsed,
        };
        let session = super::find_session(&sessions, from, src)
            .map_err(|ambiguous| ErrorData::invalid_params(ambiguous, None))?
            .ok_or_else(|| {
                let scope = from.map_or(String::new(), |harness| format!(" {harness}"));
                ErrorData::invalid_params(format!("no local{scope} session matches `{src}`"), None)
            })?;
        let common = session.read().map_err(|error| {
            ErrorData::internal_error(format!("reading session `{src}`: {error}"), None)
        })?;
        render_read_session(src, &common, span_req.as_ref())
    }
}

fn render_read_session(
    src: &str,
    common: &txcript::Transcript<txcript::Common>,
    span_req: Option<&crate::fragment::SpanReq>,
) -> Result<String, ErrorData> {
    let total = common.body.len();
    let span = match span_req {
        Some(req) => req
            .resolve(total)
            .map_err(|error| ErrorData::invalid_params(error, None))?,
        None => Span(0..total),
    };
    // `resolve` bounds-checked against `total`, so the render lands.
    let rendered = text::to_text_fragment(common, &span).ok_or_else(|| {
        ErrorData::internal_error(
            format!("range is out of bounds — the session has {total} messages"),
            None,
        )
    })?;
    // An explicitly requested single message is served whatever its size
    // — there is no smaller range left to suggest.
    if rendered.len() > READ_BUDGET && span.0.len() > 1 {
        return Err(ErrorData::invalid_params(
            over_budget(src, common, &span, rendered.len()),
            None,
        ));
    }
    Ok(rendered)
}

/// The refusal for an over-budget read: how big it was, and concrete
/// `#range` chunks — sized from each message's actual rendered bytes — the
/// caller can request instead.
fn over_budget(
    src: &str,
    common: &txcript::Transcript<txcript::Common>,
    span: &Span,
    rendered: usize,
) -> String {
    let sizes: Vec<usize> = span
        .0
        .clone()
        .map(|i| text::to_text_fragment(common, &Span(i..i + 1)).map_or(0, |s| s.len()))
        .collect();
    let chunks = chunk_ranges(&sizes, span.0.start, READ_BUDGET);
    let shown = chunks
        .iter()
        .take(12)
        .map(|r| match r.len() {
            1 => format!("`{src}#{}`", r.start + 1),
            _ => format!("`{src}#{}-{}`", r.start + 1, r.end),
        })
        .collect::<Vec<_>>()
        .join(", ");
    let more = if chunks.len() > 12 { ", …" } else { "" };
    format!(
        "session `{src}` renders to {rendered} bytes, over the {READ_BUDGET}-byte read budget — \
         read it in ranges: {shown}{more}"
    )
}

/// Consecutive message ranges (absolute indices, the first starting at
/// `start`) each fitting `budget` rendered bytes; a message alone over the
/// budget gets its own range.
fn chunk_ranges(sizes: &[usize], start: usize, budget: usize) -> Vec<std::ops::Range<usize>> {
    let mut chunks = Vec::new();
    let mut lo = 0usize;
    let mut acc = 0usize;
    for (i, &size) in sizes.iter().enumerate() {
        if i > lo && acc + size > budget {
            chunks.push(start + lo..start + i);
            lo = i;
            acc = 0;
        }
        acc += size;
    }
    if lo < sizes.len() {
        chunks.push(start + lo..start + sizes.len());
    }
    chunks
}

// rmcp's macro emits an `async fn` with no `.await`; clippy 1.98's
// `unused_async_trait_impl` fires on that generated code, not on ours.
// `unknown_lints` keeps older clippies quiet about the newer lint name.
#[allow(unknown_lints, clippy::unused_async_trait_impl)]
#[tool_handler(router = self.tool_router)]
impl ServerHandler for SessionServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(
                Implementation::new("txcript", env!("CARGO_PKG_VERSION"))
                    .with_title("txcript session server")
                    .with_description("Find, search, and read local coding-agent sessions"),
            )
            .with_instructions(
                "Use list_sessions to browse (`limit`/`offset` page it), search_sessions to find content, and read_session to retrieve token-optimized context — append `#5-12` to a session ref to read a message range, and expect over-budget reads to come back with suggested ranges. Omitted `from` and `cwd` filters include all harnesses and directories.",
            )
    }
}

/// The same gate as the CLI's `--from`: live sources are read only when the
/// request names it; an omitted `from` scans local harnesses alone.
fn discover_scoped(from: Option<HarnessId>) -> Result<Vec<local::Session>, ErrorData> {
    local::discover_scoped(from).map_err(|error| ErrorData::internal_error(error.to_string(), None))
}

fn parse_from(from: Option<&str>) -> Result<Option<HarnessId>, ErrorData> {
    from.map(str::parse).transpose().map_err(|error| {
        ErrorData::invalid_params(
            format!("{error}; expected one of: {}", super::HARNESSES),
            None,
        )
    })
}

/// Serve the session tools over stdio until the client disconnects.
///
/// # Errors
/// When the stdio service cannot start or fails while running.
pub async fn serve(cache: Option<PathBuf>) -> Result<ExitCode, String> {
    let service = SessionServer::new(cache)
        .serve(stdio())
        .await
        .map_err(|error| format!("starting MCP stdio server: {error}"))?;
    service
        .waiting()
        .await
        .map_err(|error| format!("running MCP stdio server: {error}"))?;
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_exactly_the_three_session_tools() {
        let mut names = SessionServer::new(None)
            .tool_router
            .list_all()
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect::<Vec<_>>();
        names.sort();
        assert_eq!(names, ["list_sessions", "read_session", "search_sessions"]);
    }

    #[test]
    fn list_and_search_schemas_expose_cli_filters_as_optional() {
        let list = SessionServer::list_sessions_tool_attr();
        assert!(list.input_schema["properties"].get("from").is_some());
        assert!(list.input_schema["properties"].get("cwd").is_some());
        assert!(list.input_schema["properties"].get("limit").is_some());
        assert!(list.input_schema["properties"].get("offset").is_some());
        assert!(list.input_schema.get("required").is_none());

        let search = SessionServer::search_sessions_tool_attr();
        assert!(search.input_schema["properties"].get("pattern").is_some());
        assert!(search.input_schema["properties"].get("from").is_some());
        assert!(search.input_schema["properties"].get("cwd").is_some());
        assert_eq!(
            search.input_schema["required"],
            serde_json::json!(["pattern"])
        );

        let read = SessionServer::read_session_tool_attr();
        assert_eq!(read.input_schema["required"], serde_json::json!(["id"]));
    }

    #[test]
    fn published_schemas_carry_no_unknown_format() {
        // Strict MCP clients warn on every `format` they do not recognize.
        // `usize` and `u32` fields, and the `$defs/Range_of_uint` a
        // `Range<usize>` generates, are where schemars introduces them.
        fn formats(schema: &serde_json::Value, found: &mut Vec<String>) {
            match schema {
                serde_json::Value::Object(object) => {
                    if let Some(format) = object.get("format").and_then(|f| f.as_str()) {
                        found.push(format.to_string());
                    }
                    for value in object.values() {
                        formats(value, found);
                    }
                }
                serde_json::Value::Array(items) => {
                    for item in items {
                        formats(item, found);
                    }
                }
                _ => {}
            }
        }

        let mut found = Vec::new();
        for tool in SessionServer::new(None).tool_router.list_all() {
            formats(&serde_json::json!(tool.input_schema), &mut found);
            if let Some(output) = tool.output_schema.as_ref() {
                formats(&serde_json::json!(output), &mut found);
            }
        }
        let unknown: Vec<_> = found
            .iter()
            .filter(|format| !STANDARD_FORMATS.contains(&format.as_str()))
            .collect();
        assert!(
            unknown.is_empty(),
            "non-standard formats published: {unknown:?}"
        );
    }

    #[test]
    fn stripping_a_format_keeps_the_constraint() {
        // `format` goes; `type` and `minimum` — the parts a validator acts
        // on — stay. A standard format is left in place.
        let mut schema = serde_json::json!({
            "type": "object",
            "properties": {
                "offset": {"type": "integer", "format": "uint", "minimum": 0},
                "seen": {"type": "string", "format": "date-time"},
                "format": {"type": "string"}
            },
            "$defs": {
                "Range_of_uint": {
                    "properties": {"end": {"type": "integer", "format": "uint", "minimum": 0}}
                }
            }
        });
        let serde_json::Value::Object(ref mut object) = schema else {
            return;
        };
        strip_nonstandard_formats(object);

        let offset = &schema["properties"]["offset"];
        assert!(offset.get("format").is_none());
        assert_eq!(offset["type"], "integer");
        assert_eq!(offset["minimum"], 0);
        assert_eq!(schema["properties"]["seen"]["format"], "date-time");
        assert_eq!(schema["properties"]["format"]["type"], "string");
        assert!(
            schema["$defs"]["Range_of_uint"]["properties"]["end"]
                .get("format")
                .is_none()
        );
    }

    #[test]
    fn chunking_packs_messages_under_the_budget() {
        // Two forty-byte messages fit a 100-byte budget; the third spills.
        assert_eq!(chunk_ranges(&[40, 40, 40], 0, 100), [0..2, 2..3]);
        // A message alone over the budget gets its own range rather than
        // blocking the split.
        assert_eq!(chunk_ranges(&[10, 500, 10], 0, 100), [0..1, 1..2, 2..3]);
        // Absolute indices honor the span's start.
        assert_eq!(chunk_ranges(&[60, 60], 4, 100), [4..5, 5..6]);
        // Everything fitting the budget means one whole-span range.
        assert_eq!(chunk_ranges(&[10, 10], 0, 100), vec![0..2]);
    }

    #[test]
    fn list_sessions_refuses_claude_chat() {
        let error = SessionServer::new(None)
            .list_sessions(Parameters(ListSessionsRequest {
                from: Some("claude_chat".into()),
                cwd: None,
                limit: None,
                offset: None,
            }))
            .err()
            .unwrap_or_else(|| panic!("claude_chat listing is refused"));
        assert!(error.message.contains("does not enumerate claude_chat"));
    }

    #[test]
    fn list_sessions_refuses_chatgpt() {
        let error = SessionServer::new(None)
            .list_sessions(Parameters(ListSessionsRequest {
                from: Some("chatgpt".into()),
                cwd: None,
                limit: None,
                offset: None,
            }))
            .err()
            .unwrap_or_else(|| panic!("chatgpt listing is refused"));
        assert!(error.message.contains("does not enumerate chatgpt"));
    }

    #[test]
    fn omitted_from_means_every_harness_and_aliases_still_work() {
        assert_eq!(parse_from(None).ok(), Some(None));
        assert_eq!(
            parse_from(Some("claude")).ok(),
            Some(Some(HarnessId::ClaudeCode))
        );
        assert!(parse_from(Some("not-a-harness")).is_err());
    }
}
