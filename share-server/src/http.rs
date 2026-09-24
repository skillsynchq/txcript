//! Routes and handlers.
//!
//! Each handler is the same four steps: identify, HEAD, [`decide`], execute.
//! No handler compares a principal to an owner or chooses a status code for
//! an authorization outcome — those come back in the [`Plan`].

use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, Query, State as AxumState};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, put};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use txcript_share_core::identity::{Headers, Identity};
use txcript_share_core::plan::{ObjectFacts, Plan, Precondition, Request, Status, decide};
use txcript_share_core::policy::{Action, ListScope, Policy, Target};
use txcript_share_core::{Key, Principal};
use txcript_share_store::{Cursor, ObjectStore, StoreError, attrs_for_document};

use crate::config::Limits;
use crate::store::Store;

/// Everything a handler needs, shared by reference.
pub struct State {
    pub identity: Box<dyn Identity>,
    pub policy: Box<dyn Policy>,
    pub store: Store,
    pub limits: Limits,
}

/// The attribute under which an object's team is stored, for policies that
/// decide by it.
const TEAM_ATTR: &str = "team";

pub fn router(state: Arc<State>) -> Router {
    Router::new()
        .route("/s", get(list))
        .route("/s/{session}", put(publish))
        .route("/s/{owner}/{session}", get(read).delete(remove))
        .with_state(state)
}

// --- handlers ---------------------------------------------------------

async fn publish(
    AxumState(state): AxumState<Arc<State>>,
    Path(session): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let who = match principal(&state, &headers) {
        Ok(who) => who,
        Err(rejection) => return rejection.into_response(),
    };

    if body.len() > state.limits.max_document_bytes {
        return fail(StatusCode::PAYLOAD_TOO_LARGE, "document too large");
    }
    let Ok(text) = std::str::from_utf8(&body) else {
        return fail(StatusCode::BAD_REQUEST, "body is not valid UTF-8");
    };
    let Some(doc) = parse_simple(text) else {
        return fail(
            StatusCode::BAD_REQUEST,
            "body is not a Simple transcript document",
        );
    };

    // The key is derived from the principal inside `decide`, so a HEAD needs
    // the same derivation. An invalid session is the core's to reject.
    let facts = match Key::new(who.id.clone(), &session) {
        Some(key) => match head(&state, &key).await {
            Ok(facts) => facts,
            Err(rejection) => return rejection.into_response(),
        },
        None => None,
    };

    let plan = decide(
        &Request::Publish {
            session,
            if_match: header_value(&headers, header::IF_MATCH.as_str()),
        },
        &who,
        facts.as_ref(),
        state.policy.as_ref(),
    );

    let Plan::WriteObject { key, precondition } = &plan else {
        return execute(&state, &who, plan).await;
    };
    let created = matches!(precondition, Precondition::IfAbsent);
    match state
        .store
        .put(key, &body, &attrs_for_document(&doc), precondition)
        .await
    {
        Ok(version) => (
            if created {
                StatusCode::CREATED
            } else {
                StatusCode::OK
            },
            axum::Json(json!({ "slug": key.to_slug(), "etag": version.as_str() })),
        )
            .into_response(),
        Err(StoreError::PreconditionFailed) => fail(
            StatusCode::PRECONDITION_FAILED,
            "transcript changed since it was read",
        ),
        Err(error) => backend(&error),
    }
}

async fn read(
    AxumState(state): AxumState<Arc<State>>,
    Path((owner, session)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let who = match principal(&state, &headers) {
        Ok(who) => who,
        Err(rejection) => return rejection.into_response(),
    };
    let Some(key) = Key::parse(&format!("{owner}/{session}")) else {
        return fail(StatusCode::BAD_REQUEST, "malformed slug");
    };
    let facts = match head(&state, &key).await {
        Ok(facts) => facts,
        Err(rejection) => return rejection.into_response(),
    };
    let plan = decide(
        &Request::Read { key },
        &who,
        facts.as_ref(),
        state.policy.as_ref(),
    );
    execute(&state, &who, plan).await
}

async fn remove(
    AxumState(state): AxumState<Arc<State>>,
    Path((owner, session)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let who = match principal(&state, &headers) {
        Ok(who) => who,
        Err(rejection) => return rejection.into_response(),
    };
    let Some(key) = Key::parse(&format!("{owner}/{session}")) else {
        return fail(StatusCode::BAD_REQUEST, "malformed slug");
    };
    let facts = match head(&state, &key).await {
        Ok(facts) => facts,
        Err(rejection) => return rejection.into_response(),
    };
    let plan = decide(
        &Request::Delete { key },
        &who,
        facts.as_ref(),
        state.policy.as_ref(),
    );
    execute(&state, &who, plan).await
}

#[derive(Debug, Deserialize)]
struct ListQuery {
    #[serde(default)]
    owner: Option<String>,
}

async fn list(
    AxumState(state): AxumState<Arc<State>>,
    Query(query): Query<ListQuery>,
    headers: HeaderMap,
) -> Response {
    let who = match principal(&state, &headers) {
        Ok(who) => who,
        Err(rejection) => return rejection.into_response(),
    };
    let scope = if query.owner.as_deref() == Some("me") {
        ListScope::Mine
    } else {
        ListScope::Everyone
    };
    let plan = decide(&Request::List { scope }, &who, None, state.policy.as_ref());
    execute(&state, &who, plan).await
}

// --- plan execution ---------------------------------------------------

#[derive(Debug, Serialize)]
struct Entry {
    slug: String,
    etag: String,
    #[serde(flatten)]
    attrs: serde_json::Map<String, Value>,
}

async fn execute(state: &State, who: &Principal, plan: Plan) -> Response {
    match plan {
        Plan::Reject(Status(status), reason) => {
            let code = StatusCode::from_u16(status).unwrap_or(StatusCode::FORBIDDEN);
            fail(code, reason)
        }

        Plan::ReadObject(key) => match state.store.get(&key).await {
            // Decided against a HEAD; the object can vanish between the two,
            // and that is a 404 rather than an error.
            Ok(None) => fail(StatusCode::NOT_FOUND, "no such transcript"),
            Ok(Some(object)) => (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, "application/json".to_string()),
                    (header::ETAG, object.meta.version.as_str().to_string()),
                ],
                object.body,
            )
                .into_response(),
            Err(error) => backend(&error),
        },

        Plan::DeleteObject(key) => match state.store.delete(&key, &Precondition::None).await {
            Ok(()) => StatusCode::NO_CONTENT.into_response(),
            Err(error) => backend(&error),
        },

        Plan::List(list) => {
            let prefix = list.prefix.as_store_prefix();
            let mut entries = Vec::new();
            let mut cursor: Option<Cursor> = None;
            loop {
                let page = match state
                    .store
                    .list(&prefix, cursor.as_ref(), state.limits.page_size)
                    .await
                {
                    Ok(page) => page,
                    Err(error) => return backend(&error),
                };
                for meta in page.objects {
                    // A policy whose read rule is not a prefix decides each
                    // entry; skipping this would list what it denies on read.
                    if list.authorize_each {
                        let mut target = Target::new(meta.key.clone());
                        target.team = meta.attrs.get(TEAM_ATTR).map(str::to_string);
                        if !state
                            .policy
                            .authorize(who, Action::Read, &target)
                            .is_allowed()
                        {
                            continue;
                        }
                    }
                    entries.push(Entry {
                        slug: meta.key.to_slug(),
                        etag: meta.version.as_str().to_string(),
                        attrs: meta
                            .attrs
                            .iter()
                            .map(|(k, v)| (k.to_string(), Value::String(v.to_string())))
                            .collect(),
                    });
                }
                match page.next {
                    Some(next) => cursor = Some(next),
                    None => break,
                }
            }
            (StatusCode::OK, axum::Json(json!({ "sessions": entries }))).into_response()
        }

        // Writes are executed by `publish`, which owns the body.
        Plan::WriteObject { .. } => fail(StatusCode::INTERNAL_SERVER_ERROR, "unexpected plan"),
    }
}

// --- helpers ----------------------------------------------------------

fn principal(state: &State, headers: &HeaderMap) -> Result<Principal, Rejection> {
    let collected: Headers = headers
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_string(), value.to_string()))
        })
        .collect();
    match state.identity.principal(&collected) {
        Ok(Some(who)) => Ok(who),
        Ok(None) => Err(Rejection(StatusCode::UNAUTHORIZED, "unauthenticated")),
        // The check could not be performed. That is an outage, not a user
        // error, and must not read as 401 to whoever is watching.
        Err(error) => {
            eprintln!("identity backend failed: {}", error.0);
            Err(Rejection(
                StatusCode::SERVICE_UNAVAILABLE,
                "identity backend unavailable",
            ))
        }
    }
}

async fn head(state: &State, key: &Key) -> Result<Option<ObjectFacts>, Rejection> {
    match state.store.head(key).await {
        Ok(found) => Ok(found.map(|meta| ObjectFacts {
            version: meta.version.as_str().to_string(),
            team: meta.attrs.get(TEAM_ATTR).map(str::to_string),
        })),
        Err(error) => Err(backend_rejection(&error)),
    }
}

fn header_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

/// A refusal: a status and a fixed reason, rendered at the edge.
///
/// Small by construction, and it keeps the helpers out of the business of
/// building responses — they decide, the handler renders.
#[derive(Debug, Clone, Copy)]
struct Rejection(StatusCode, &'static str);

impl IntoResponse for Rejection {
    fn into_response(self) -> Response {
        let Rejection(status, reason) = self;
        (status, axum::Json(json!({ "error": reason }))).into_response()
    }
}

fn fail(status: StatusCode, reason: &'static str) -> Response {
    Rejection(status, reason).into_response()
}

fn backend(error: &StoreError) -> Response {
    backend_rejection(error).into_response()
}

fn backend_rejection(error: &StoreError) -> Rejection {
    // Never surface the backend's own message to the caller: it carries
    // paths, bucket names, and upstream detail. It still belongs in the log.
    eprintln!("{error}");
    Rejection(StatusCode::SERVICE_UNAVAILABLE, "storage unavailable")
}

/// Format sniffing, the same contract the on-disk stores honour: a name or a
/// content type does not identify a transcript, the shape does.
fn parse_simple(body: &str) -> Option<Value> {
    let value: Value = serde_json::from_str(body).ok()?;
    let is_document = value.is_object() && value.get("messages").is_some_and(Value::is_array);
    is_document.then_some(value)
}
