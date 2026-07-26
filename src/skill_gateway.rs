//! Client for the official WeRead Skill Gateway
//! (`docs/design.md` §"关键 API 端点一览") — a single POST endpoint that
//! multiplexes every read-only account/shelf/progress/notes/review
//! endpoint via an `api_name` field in the JSON body, authenticated with
//! a per-user Bearer API key (minted during login — see
//! `login::complete`'s apikeyGet call).
//!
//! This does NOT serve full chapter body content — that's the separate,
//! unauthenticated-by-Bearer Web reader flow (signed requests, content
//! decode) planned for a later phase.

use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

const GATEWAY_URL: &str = "https://i.weread.qq.com/api/agent/gateway";
const SKILL_VERSION: &str = "1.0.3";

/// Calls one gateway `api_name` with extra top-level JSON fields (business
/// parameters are top-level in the request body, not nested — see
/// docs/design.md's API notes) and decodes the response as `T`.
pub fn call<T: DeserializeOwned>(
    agent: &ureq::Agent,
    api_key: &str,
    api_name: &str,
    params: Value,
) -> Result<T, Box<dyn std::error::Error>> {
    let mut body = match params {
        Value::Object(map) => map,
        Value::Null => Map::new(),
        other => return Err(format!("skill_gateway params must be an object, got {other}").into()),
    };
    body.insert("api_name".to_string(), Value::String(api_name.to_string()));
    body.insert("skill_version".to_string(), Value::String(SKILL_VERSION.to_string()));

    // Read the body even on 4xx/5xx. ureq turns a bad status into an
    // Err whose body is discarded, and WeRead puts the *reason* in that
    // body — a bare "http status: 403" cost real debugging time once.
    let mut resp = agent
        .post(GATEWAY_URL)
        .config()
        .http_status_as_error(false)
        .build()
        .header("Authorization", format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .send_json(Value::Object(body))?;

    let status = resp.status();
    let text = resp.body_mut().read_to_string()?;
    if !status.is_success() {
        return Err(format!("{api_name} -> HTTP {status}: {}", text.trim()).into());
    }
    // `upgrade_info` just means a newer Skill spec version exists — the
    // response body still carries this call's real, complete data (see
    // the real payload we hit during development: a full shelf response
    // with `upgrade_info` tacked on as a sibling field, not a
    // replacement for the actual data). Warn once and keep parsing;
    // don't treat it as a request failure.
    if let Ok(upgrade) = serde_json::from_str::<Value>(&text)
        && let Some(info) = upgrade.get("upgrade_info")
    {
        eprintln!("skill_gateway: WeRead reports a newer skill version is available: {info}");
    }
    Ok(serde_json::from_str(&text)?)
}
