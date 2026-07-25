//! QR login, ported from koplugin's lib/qr_login.lua protocol (the UI
//! chrome — QR widget, retry dialogs — is KOReader-specific and dropped;
//! only the wire protocol is kept):
//!
//! 1. GET the skills page once, just to pick up initial cookies.
//! 2. GET getLoginUid -> a `uid`; show the user
//!    `https://weread.qq.com/web/confirm?uid=<uid>` as a QR code (or, for
//!    this CLI test binary, just print the URL — open it on a phone
//!    that's already logged into WeRead to do the equivalent of scanning).
//! 3. Long-poll getLoginInfo?uid=<uid> until it reports success, asks for
//!    a 4-digit OTP, or expires.
//! 4. On success, mint a *fresh* cookie jar from the login response
//!    (wr_vid/wr_skey/wr_rt/wr_ql) — deliberately not reusing whatever
//!    cookies existed before, so a re-login can't leak a previous
//!    account's session (same reasoning as qr_login.lua's comment).
//! 5. Fetch userInfo + the official Skill Gateway API key with that fresh
//!    session, retrying up to 3x on a transient 401 (observed in
//!    koplugin: the session can be momentarily unauthorized right after
//!    login completes).

use crate::cookie::CookieJar;
use serde::{Deserialize, Serialize};
use std::time::Duration;

const BASE_URL: &str = "https://weread.qq.com";
const SKILLS_PAGE_URL: &str = "https://weread.qq.com/r/weread-skills";
const LOGIN_UID_URL: &str = "https://weread.qq.com/api/auth/getLoginUid";
const LOGIN_INFO_URL: &str = "https://weread.qq.com/api/auth/getLoginInfo";
const USER_INFO_URL: &str = "https://weread.qq.com/api/userInfo";
const API_KEY_URL: &str = "https://weread.qq.com/api/skills/apikeyGet?only_show=1";

// koplugin blocks each poll for 5s server-side and gives up on the HTTP
// request entirely after 8s; a plain timeout (not an error response) just
// means "still waiting", so the caller polls again immediately.
const POLL_TIMEOUT: Duration = Duration::from_secs(8);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub name: String,
    pub user_vid: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub cookies: CookieJar,
    pub api_key: String,
    pub account: Account,
}

#[derive(Deserialize)]
struct LoginUidResponse {
    uid: String,
}

/// WeRead's JSON is inconsistent about whether ID-shaped fields
/// (`webLoginVid` in particular — confirmed on a real account to come
/// back as a bare JSON integer, not a string) are strings or numbers.
/// Accept either and normalize to a `String`, since every downstream use
/// treats these as opaque tokens anyway.
fn string_or_number<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrNumber {
        String(String),
        Number(serde_json::Number),
    }
    Ok(match Option::<StringOrNumber>::deserialize(deserializer)? {
        Some(StringOrNumber::String(s)) => s,
        Some(StringOrNumber::Number(n)) => n.to_string(),
        None => String::new(),
    })
}

#[derive(Deserialize, Default)]
struct LoginInfoResponse {
    #[serde(default)]
    succeed: bool,
    #[serde(rename = "logicCode", default)]
    logic_code: String,
    #[serde(rename = "webLoginVid", default, deserialize_with = "string_or_number")]
    web_login_vid: String,
    #[serde(rename = "accessToken", default, deserialize_with = "string_or_number")]
    access_token: String,
    #[serde(rename = "refreshToken", default, deserialize_with = "string_or_number")]
    refresh_token: String,
}

#[derive(Deserialize)]
struct UserInfoResponse {
    #[serde(default)]
    name: String,
}

#[derive(Deserialize)]
struct ApiKeyResponse {
    #[serde(default)]
    apikey: String,
}

pub enum PollOutcome {
    /// Timed out waiting, or the server is still waiting on the user —
    /// poll again.
    Pending,
    /// The account requires a 4-digit verification code shown on the
    /// WeRead app; call `poll` again with `otp = Some(code)`.
    NeedOtp { retry_message: Option<String> },
    /// The QR code (or the OTP step) expired; `begin` must be called
    /// again for a fresh `uid`.
    Expired,
    Done(LoginResult),
}

/// The raw fields from a successful getLoginInfo — not yet a full
/// `Session` because userInfo/apikeyGet haven't been called yet.
pub struct LoginResult {
    web_login_vid: String,
    access_token: String,
    refresh_token: String,
}

fn is_timeout_error(err: &ureq::Error) -> bool {
    matches!(err, ureq::Error::Timeout(_))
}

pub fn agent() -> ureq::Agent {
    ureq::Agent::config_builder().timeout_global(Some(POLL_TIMEOUT)).build().into()
}

/// Step 1+2: picks up initial cookies from the skills page, then requests
/// a login `uid`. Returns the uid plus the cookie jar accumulated so far
/// (must be threaded through to `poll`).
pub fn begin(agent: &ureq::Agent) -> Result<(String, CookieJar), Box<dyn std::error::Error>> {
    let mut cookies = CookieJar::new();

    let resp = agent
        .get(SKILLS_PAGE_URL)
        .header("Accept", "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8")
        .header("Referer", format!("{BASE_URL}/"))
        .call()?;
    cookies.merge_from_response(&resp);

    let mut resp = agent
        .get(LOGIN_UID_URL)
        .header("Accept", "application/json, text/plain, */*")
        .header("Referer", SKILLS_PAGE_URL)
        .header("Cookie", cookies.to_header())
        .call()?;
    cookies.merge_from_response(&resp);
    let data: LoginUidResponse = resp.body_mut().read_json()?;

    Ok((data.uid, cookies))
}

pub fn confirm_url(uid: &str) -> String {
    format!("{BASE_URL}/web/confirm?uid={}", urlencoding::encode(uid))
}

/// Step 3: one long-poll round. `cookies` accumulates Set-Cookie headers
/// across calls (pass the same jar back in each time).
pub fn poll(
    agent: &ureq::Agent,
    uid: &str,
    cookies: &mut CookieJar,
    otp: Option<&str>,
) -> Result<PollOutcome, Box<dyn std::error::Error>> {
    let mut url = format!("{LOGIN_INFO_URL}?uid={}&otp", urlencoding::encode(uid));
    if let Some(otp) = otp {
        url.push('=');
        url.push_str(&urlencoding::encode(otp));
    }

    let resp = agent
        .get(&url)
        .header("Accept", "application/json, text/plain, */*")
        .header("Referer", SKILLS_PAGE_URL)
        .header("Cookie", cookies.to_header())
        .call();

    let mut resp = match resp {
        Ok(resp) => resp,
        Err(e) if is_timeout_error(&e) => return Ok(PollOutcome::Pending),
        Err(e) => return Err(e.into()),
    };
    cookies.merge_from_response(&resp);
    let data: LoginInfoResponse = resp.body_mut().read_json()?;

    if data.succeed {
        return Ok(PollOutcome::Done(LoginResult {
            web_login_vid: data.web_login_vid,
            access_token: data.access_token,
            refresh_token: data.refresh_token,
        }));
    }

    Ok(match data.logic_code.as_str() {
        "NEED_OTP" => PollOutcome::NeedOtp { retry_message: None },
        "OTP_NOT_MATCH" => {
            PollOutcome::NeedOtp { retry_message: Some("验证码不对，再试一次".to_string()) }
        }
        "LOGIN_TIMEOUT" | "OTP_EXPIRED" => PollOutcome::Expired,
        _ => PollOutcome::Pending,
    })
}

// koplugin's own comment on this same retry (qr_login.lua's
// _authenticated_get): the session can be *momentarily* unauthorized
// right after login completes — presumably replication lag on WeRead's
// side between where the login write lands and where these reads check.
// 3 attempts / 500ms was observed (on a real account, this session) to
// not always be enough; give it more attempts and a longer backoff.
const AUTH_RETRY_ATTEMPTS: u32 = 8;
const AUTH_RETRY_DELAY: Duration = Duration::from_millis(1500);

fn authenticated_get(
    agent: &ureq::Agent,
    url: &str,
    cookies: &mut CookieJar,
    web_login_vid: &str,
    access_token: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let mut last_err = None;
    for attempt in 0..AUTH_RETRY_ATTEMPTS {
        let resp = agent
            .get(url)
            .header("Accept", "application/json, text/plain, */*")
            .header("Referer", SKILLS_PAGE_URL)
            .header("Cookie", cookies.to_header())
            .header("X-Vid", web_login_vid)
            .header("X-Skey", access_token)
            .call();
        match resp {
            Ok(mut resp) => {
                cookies.merge_from_response(&resp);
                return Ok(resp.body_mut().read_to_string()?);
            }
            Err(ureq::Error::StatusCode(401)) if attempt + 1 < AUTH_RETRY_ATTEMPTS => {
                eprintln!(
                    "  ({url} still 401, attempt {}/{AUTH_RETRY_ATTEMPTS}, retrying...)",
                    attempt + 1
                );
                std::thread::sleep(AUTH_RETRY_DELAY);
                last_err = Some("401 Unauthorized".to_string());
            }
            Err(e) => return Err(e.into()),
        }
    }
    Err(last_err.unwrap_or_default().into())
}

/// Step 4+5: turns a successful `LoginResult` into a full `Session` by
/// minting a fresh cookie jar and fetching the account name + official
/// Skill Gateway API key.
pub fn complete(agent: &ureq::Agent, result: LoginResult) -> Result<Session, Box<dyn std::error::Error>> {
    let LoginResult { web_login_vid, access_token, refresh_token } = result;
    if web_login_vid.is_empty() || access_token.is_empty() {
        return Err("QR login response is missing account credentials".into());
    }

    let mut cookies = CookieJar::new();
    cookies.set("wr_vid", &web_login_vid);
    cookies.set("wr_skey", &access_token);
    cookies.set("wr_ql", "0");
    if !refresh_token.is_empty() {
        cookies.set("wr_rt", urlencoding::encode(&refresh_token).into_owned());
    }

    eprintln!("  fetching userInfo...");
    let user_url = format!("{USER_INFO_URL}?userVid={}", urlencoding::encode(&web_login_vid));
    let user_info_json = authenticated_get(agent, &user_url, &mut cookies, &web_login_vid, &access_token)?;
    let user_info: UserInfoResponse = serde_json::from_str(&user_info_json)?;

    eprintln!("  fetching skill API key (this account must have WeRead Skill enabled)...");
    let mut api_key = String::new();
    for attempt in 0..AUTH_RETRY_ATTEMPTS {
        let api_key_json = authenticated_get(agent, API_KEY_URL, &mut cookies, &web_login_vid, &access_token)?;
        let parsed: ApiKeyResponse = serde_json::from_str(&api_key_json)?;
        if !parsed.apikey.is_empty() {
            api_key = parsed.apikey;
            break;
        }
        if attempt + 1 < AUTH_RETRY_ATTEMPTS {
            eprintln!(
                "  (apikey still empty, attempt {}/{AUTH_RETRY_ATTEMPTS}, retrying...)",
                attempt + 1
            );
            std::thread::sleep(AUTH_RETRY_DELAY);
        }
    }
    if api_key.is_empty() {
        return Err(
            "No official API key was returned. Open the WeRead app → 我 → 设置 → 微信读书 Skill \
             → 获取 API Key, then log in again."
                .into(),
        );
    }

    Ok(Session {
        cookies,
        api_key,
        account: Account {
            name: if user_info.name.is_empty() { "(unknown)".to_string() } else { user_info.name },
            user_vid: web_login_vid,
        },
    })
}
