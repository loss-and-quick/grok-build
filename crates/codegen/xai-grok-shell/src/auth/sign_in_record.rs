//! The plugin sign-in this machine last completed, remembered across restarts.
//!
//! A first-party login leaves evidence of itself: the token lands in
//! `auth.json`, and the next launch re-derives the identity from it (a
//! `cached_token` method that [`credential_method_id`] normalizes back to the
//! interactive method that owns it). A plugin sign-in leaves no such evidence
//! *here* — the credential lives behind the plugin's own seam, the
//! `AuthManager` never sees it, and nothing on disk names the plugin or the
//! account. Re-deriving the identity from what the shell can observe therefore
//! answers "no one is signed in" for a session that is signed in.
//!
//! So the shell records the one thing only it knows: which advertised method
//! minted the session. The record holds no credential material — a plugin name
//! and an account selector, both already declared in the plugin's manifest —
//! and it is a claim about provenance, not proof of a live token: the plugin
//! may have since dropped or expired its own. That is the same standing a
//! cached `auth.json` entry has, and it is repaired the same way, by the
//! `401` path that already knows a plugin minted the bearer.
//!
//! [`credential_method_id`]: crate::agent::auth_method::credential_method_id

use std::path::{Path, PathBuf};

use agent_client_protocol as acp;
use serde::{Deserialize, Serialize};

/// File name under `~/.grok/`.
const FILE_NAME: &str = "plugin_sign_in.json";

/// On-disk shape. Versioned so a later field can be added without a launch
/// reading garbage out of an older file.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Record {
    version: u32,
    /// The advertised method id the sign-in ran, e.g.
    /// `plugin-oauth:acme#work`. Stored whole: the account selector is half
    /// the identity when a plugin holds several accounts of one provider.
    #[serde(rename = "methodId")]
    method_id: String,
}

const VERSION: u32 = 1;

fn path_in(grok_home: &Path) -> PathBuf {
    grok_home.join(FILE_NAME)
}

/// The recorded method id, or `None` when nothing was recorded, the file is
/// unreadable, or it was written by a newer version.
///
/// Every failure is "nothing recorded": a missing identity sends the user
/// through a login they have already done, which is recoverable, while a hard
/// error here would fail a launch over a hint.
pub(crate) fn load_from(grok_home: &Path) -> Option<acp::AuthMethodId> {
    let path = path_in(grok_home);
    let contents = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "auth: unreadable plugin sign-in record");
            return None;
        }
    };
    let record: Record = serde_json::from_str(&contents)
        .inspect_err(
            |e| tracing::warn!(path = %path.display(), error = %e, "auth: malformed plugin sign-in record"),
        )
        .ok()?;
    if record.version != VERSION || record.method_id.is_empty() {
        return None;
    }
    Some(acp::AuthMethodId::new(record.method_id))
}

/// Remember `method_id` as the sign-in this machine is on. Best-effort: a
/// write failure costs the next launch its identity, not this sign-in.
pub(crate) fn record_in(grok_home: &Path, method_id: &acp::AuthMethodId) {
    let path = path_in(grok_home);
    let record = Record {
        version: VERSION,
        method_id: method_id.0.to_string(),
    };
    let write = || -> std::io::Result<()> {
        std::fs::create_dir_all(grok_home)?;
        let json = serde_json::to_string_pretty(&record).map_err(std::io::Error::other)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &path)
    };
    if let Err(e) = write() {
        tracing::warn!(path = %path.display(), error = %e, "auth: failed to record plugin sign-in");
    }
}

/// Forget the recorded sign-in. Called when another credential takes over the
/// session (a first-party login) and on logout, so a stale plugin never
/// outlives the sign-in it describes.
pub(crate) fn clear_in(grok_home: &Path) {
    let path = path_in(grok_home);
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "auth: failed to clear plugin sign-in record");
        }
    }
}

/// The directory the record lives in: the one already holding this manager's
/// `auth.json`, so a custom store — or a test's temporary home — carries its
/// own record rather than reaching for the process-wide `~/.grok`.
pub(crate) fn home_for(auth_manager: &super::AuthManager) -> &Path {
    auth_manager
        .auth_json_path()
        .parent()
        .unwrap_or_else(|| Path::new("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    // These exercise the `*_in` half only: `grok_home()` caches in a
    // process-wide `OnceLock`, so a test that resolved it would decide the
    // whole binary's answer -- and could land on the developer's real
    // `~/.grok`.

    /// The whole id round-trips, account selector included: that half is what
    /// tells two accounts of one plugin apart.
    #[test]
    fn an_account_scoped_sign_in_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let id = acp::AuthMethodId::new("plugin-oauth:acme#work");
        record_in(dir.path(), &id);
        assert_eq!(load_from(dir.path()), Some(id));
    }

    /// A plugin that declares no accounts records the bare id, and reads back
    /// as the bare id -- not as an account whose selector is empty.
    #[test]
    fn an_account_less_sign_in_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let id = acp::AuthMethodId::new("plugin-oauth:acme");
        record_in(dir.path(), &id);
        assert_eq!(load_from(dir.path()), Some(id));
    }

    /// A later sign-in replaces the earlier one: the record names the sign-in
    /// in force, not every one ever made.
    #[test]
    fn the_latest_sign_in_replaces_the_previous_one() {
        let dir = tempfile::tempdir().unwrap();
        record_in(
            dir.path(),
            &acp::AuthMethodId::new("plugin-oauth:acme#work"),
        );
        record_in(
            dir.path(),
            &acp::AuthMethodId::new("plugin-oauth:acme#personal"),
        );
        assert_eq!(
            load_from(dir.path()).map(|id| id.0.to_string()),
            Some("plugin-oauth:acme#personal".to_string())
        );
    }

    #[test]
    fn clearing_leaves_nothing_recorded() {
        let dir = tempfile::tempdir().unwrap();
        record_in(dir.path(), &acp::AuthMethodId::new("plugin-oauth:acme"));
        clear_in(dir.path());
        assert_eq!(load_from(dir.path()), None);
        // Clearing twice is not an error: logout runs whether or not a plugin
        // sign-in was ever made.
        clear_in(dir.path());
        assert_eq!(load_from(dir.path()), None);
    }

    #[test]
    fn nothing_recorded_reads_as_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load_from(dir.path()), None);
    }

    /// A corrupt or foreign-versioned file is "nothing recorded" rather than a
    /// failed launch.
    #[test]
    fn an_unusable_record_reads_as_none() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(path_in(dir.path()), "{ not json").unwrap();
        assert_eq!(load_from(dir.path()), None);
        std::fs::write(
            path_in(dir.path()),
            r#"{"version":99,"methodId":"plugin-oauth:acme"}"#,
        )
        .unwrap();
        assert_eq!(load_from(dir.path()), None);
    }
}
