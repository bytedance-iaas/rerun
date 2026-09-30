//! The session-wide TOS credential slot.
//!
//! Deployments and local configs may ship without any AK/SK (or STS-issued keys): the first
//! time a TOS operation actually needs credentials, the viewer prompts the user and stores
//! what they enter here — one global slot, entered once per session. We assume one viewer
//! user maps to one Volcengine account, so there is nothing to key the slot by.
//!
//! The slot lives in this crate (not the viewer) so [`super::client::TosClient`] can drop
//! stale credentials on an authorization failure, which makes the viewer prompt again.

use parking_lot::Mutex;

/// Credentials without an endpoint — the endpoint is chosen per open (region dropdown, config).
#[derive(Clone)]
pub struct SessionCredentials {
    pub access_key: String,
    pub secret_key: String,

    /// Non-empty when the user entered STS temporary credentials instead of a long-term pair.
    pub session_token: String,
}

static SLOT: Mutex<Option<SessionCredentials>> = Mutex::new(None);

/// Remember the credentials the user entered, for the rest of this session.
/// `session_token` is empty for a long-term AK/SK pair.
pub fn store(access_key: &str, secret_key: &str, session_token: &str) {
    let access_key = access_key.trim();
    let secret_key = secret_key.trim();
    if access_key.is_empty() || secret_key.is_empty() {
        return;
    }
    *SLOT.lock() = Some(SessionCredentials {
        access_key: access_key.to_owned(),
        secret_key: secret_key.to_owned(),
        session_token: session_token.trim().to_owned(),
    });
}

/// The credentials entered earlier this session, if any.
pub fn get() -> Option<SessionCredentials> {
    SLOT.lock().clone()
}

/// Drop the stored credentials if they are the ones that just failed authorization —
/// so the next TOS open prompts for fresh ones instead of silently failing the same way.
///
/// Matching on the access key keeps a failure of deployment-configured or
/// dialog-entered credentials from wiping an unrelated stored pair.
pub(crate) fn clear_if_matches(access_key: &str) {
    let mut slot = SLOT.lock();
    if slot
        .as_ref()
        .is_some_and(|stored| stored.access_key == access_key)
    {
        *slot = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialized because the slot is process-global and tests run in parallel.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn blank_credentials_are_not_stored() {
        let _guard = TEST_LOCK.lock();
        *SLOT.lock() = None;
        store("  ", "sk", "");
        store("ak", "", "");
        assert!(get().is_none());
    }

    #[test]
    fn clear_only_hits_the_matching_key() {
        let _guard = TEST_LOCK.lock();
        store("ak-1", "sk-1", "token-1");
        clear_if_matches("ak-other");
        assert!(get().is_some());
        clear_if_matches("ak-1");
        assert!(get().is_none());
    }
}
