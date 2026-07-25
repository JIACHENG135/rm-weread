//! Persists a logged-in `login::Session` to disk so the QR flow only has
//! to happen once. On-device this belongs at
//! `/home/root/.local/share/rm-weread/session.json` (the path convention
//! docs/design.md already committed to, matching REweread's own
//! layout); the path is a parameter here so local/CLI testing doesn't
//! need to touch that.

use crate::login::Session;
use std::fs;
use std::path::Path;

pub fn save(path: &Path, session: &Session) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(session).unwrap_or_default();
    // Atomic write-then-rename, same pattern as rm-agent's daemons use for
    // files another process might read concurrently — cheap enough to do
    // by default even though nothing polls this file yet.
    let tmp_path = path.with_extension("json.tmp");
    fs::write(&tmp_path, json)?;
    fs::rename(&tmp_path, path)
}

pub fn load(path: &Path) -> Option<Session> {
    let contents = fs::read_to_string(path).ok()?;
    serde_json::from_str(&contents).ok()
}
