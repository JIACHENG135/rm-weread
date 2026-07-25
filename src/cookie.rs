//! Minimal cookie jar: just name=value pairs, no attribute tracking
//! (Path/Domain/Expires/...). WeRead only ever cares about a handful of
//! specific cookie names (wr_vid, wr_skey, wr_rt, wr_ql) that we set and
//! read explicitly, so there's nothing here that a general-purpose
//! cookie-store crate would buy us — see koplugin's lib/cookie.lua,
//! which does the same name=value-only merge.

use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct CookieJar(BTreeMap<String, String>);

impl CookieJar {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.0.insert(name.into(), value.into());
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(String::as_str)
    }

    /// Parses the first `name=value` pair out of one Set-Cookie header
    /// value (ignoring `; Path=...`, `; HttpOnly`, etc.) and merges it in.
    pub fn merge_set_cookie(&mut self, header_value: &str) {
        let Some(pair) = header_value.split(';').next() else { return };
        let Some((name, value)) = pair.split_once('=') else { return };
        let (name, value) = (name.trim(), value.trim());
        if !name.is_empty() {
            self.0.insert(name.to_string(), value.to_string());
        }
    }

    /// Merges every `Set-Cookie` header on a response into this jar.
    pub fn merge_from_response<T>(&mut self, resp: &ureq::http::Response<T>) {
        for value in resp.headers().get_all("set-cookie") {
            if let Ok(s) = value.to_str() {
                self.merge_set_cookie(s);
            }
        }
    }

    pub fn to_header(&self) -> String {
        self.0.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("; ")
    }
}
