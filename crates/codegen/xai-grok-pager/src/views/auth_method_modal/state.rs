//! Renderer-agnostic state for the login-method picker.
//!
//! Entries are keyed by [`acp::AuthMethodId`]. Several entries may point at
//! the same provider or plugin (two accounts signed in through one provider),
//! so nothing here is deduped, indexed, or ordered by provider name.
//!
//! Every presentation *decision* — which row starts selected, what the badge
//! says, what goes in the right-hand column — is made here so a second
//! renderer (web) can lay out the same state without re-deriving the rules.

use agent_client_protocol as acp;
use xai_grok_shell::agent::auth_method::{
    AUTH_METHOD_META_ACCOUNT_LABEL, AUTH_METHOD_META_PROVIDER_LABEL, AuthMethodKind,
};

use crate::modal_window_state::ModalWindowState;
use crate::views::picker::PickerState;

/// One key/value detail row shown under an entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthMethodDetail {
    pub label: String,
    pub value: String,
}

/// One selectable login method.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthMethodEntry {
    /// Identity of this entry. Two entries can share a provider, so the
    /// method id — never the provider name — is the key.
    pub method_id: acp::AuthMethodId,
    /// Provider / plugin display name, e.g. `"Acme OAuth"`. Read from the
    /// method's `providerLabel` meta, falling back to the method name.
    pub provider_label: String,
    /// Account within `provider_label` (email, workspace, tenant), read from
    /// the method's `accountLabel` meta. `None` when the method is not scoped
    /// to an account; kept as its own field so two accounts on one provider
    /// render unambiguously instead of showing the provider name twice.
    pub account_label: Option<String>,
    /// Classification of the method id. Informational only — the picker does
    /// not prefer any particular kind.
    pub kind: AuthMethodKind,
    /// True only when this entry reflects the credential *actually in use*.
    /// The pager is not told which credential the agent authenticated with
    /// (`AuthMeta` carries no method id), so callers pass `None` for
    /// `current_method_id` today and no row claims to be current.
    pub is_current: bool,
    pub details: Vec<AuthMethodDetail>,
}

impl AuthMethodEntry {
    fn from_auth_method(
        method: &acp::AuthMethod,
        current_method_id: Option<&acp::AuthMethodId>,
    ) -> Self {
        let kind = AuthMethodKind::from_id(method.id());
        let provider_label = meta_str(method, AUTH_METHOD_META_PROVIDER_LABEL)
            .unwrap_or_else(|| method.name().to_string());
        let account_label = meta_str(method, AUTH_METHOD_META_ACCOUNT_LABEL);
        let mut details = vec![
            AuthMethodDetail {
                label: "Method".to_string(),
                value: method.id().0.to_string(),
            },
            AuthMethodDetail {
                label: "Type".to_string(),
                value: auth_kind_label(kind).to_string(),
            },
        ];
        if let Some(account) = &account_label {
            details.push(AuthMethodDetail {
                label: "Account".to_string(),
                value: account.clone(),
            });
        }
        if let Some(desc) = method.description()
            && !desc.trim().is_empty()
        {
            details.push(AuthMethodDetail {
                label: "Description".to_string(),
                value: desc.trim().to_string(),
            });
        }
        Self {
            method_id: method.id().clone(),
            provider_label,
            account_label,
            kind,
            is_current: current_method_id.is_some_and(|id| id == method.id()),
            details,
        }
    }

    /// Badge rendered right after the label; empty when there is nothing to
    /// say. Only a credential known to be in use earns a badge — a merely
    /// defaulted method does not.
    pub fn badge(&self) -> &'static str {
        if self.is_current { "current" } else { "" }
    }

    /// Right-aligned column: the account within the provider, so two entries
    /// on one provider stay distinguishable. Empty when unknown.
    ///
    /// The auth *kind* deliberately does not go here: every interactive kind
    /// is also session-based, so a kind- or session-derived label is constant
    /// across the whole list and carries no information. It is available as a
    /// detail row instead.
    pub fn right_label(&self) -> &str {
        self.account_label.as_deref().unwrap_or("")
    }
}

/// A non-blank string value from the method's `meta`, or `None`. Blank values
/// are treated as absent so a renderer never has to special-case `""`.
fn meta_str(method: &acp::AuthMethod, key: &str) -> Option<String> {
    let value = method.meta()?.get(key)?.as_str()?.trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// Outcome of one input event, for the caller to turn into an action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthMethodPickerOutcome {
    /// User picked this method.
    Chosen(acp::AuthMethodId),
    /// User dismissed the picker (Esc, `[✗]`, click outside).
    Cancelled,
    /// State changed; caller should re-render.
    Changed,
    /// Event not consumed.
    Unchanged,
}

/// State for the login-method picker.
#[derive(Debug, Clone)]
pub struct AuthMethodPickerState {
    // ---- Renderer-agnostic ----
    /// Interactive login methods, in the order the agent advertised them.
    pub entries: Vec<AuthMethodEntry>,
    /// Index into `entries`. Always in range while `entries` is non-empty.
    pub selected: usize,
    /// Whether choosing an entry should log out first (`/switch-account`)
    /// rather than plain authenticate (`/login`). Passed down explicitly by
    /// whichever dispatcher opened the picker — never re-derived from
    /// `auth_state`, which can be mid-flight.
    pub switch_account: bool,

    // ---- Renderer-side scratch (ratatui) ----
    /// List-widget state for the ratatui renderer. Persisted across frames
    /// (rather than rebuilt per draw) so the hit areas the renderer records
    /// survive long enough for the next click to be tested against them.
    pub picker: PickerState,
    /// Modal chrome state (close button, footer shortcuts, popup rect).
    pub window: ModalWindowState,
}

impl AuthMethodPickerState {
    /// Project the advertised auth methods into picker rows.
    ///
    /// Only interactive methods are kept: the others cannot be started from a
    /// picker. `preferred_method_id` is the caller's configured/defaulted
    /// method; `current_method_id` is the credential actually in use, or
    /// `None` when that is unknown.
    pub fn new(
        auth_methods: &[acp::AuthMethod],
        current_method_id: Option<&acp::AuthMethodId>,
        preferred_method_id: Option<&acp::AuthMethodId>,
        switch_account: bool,
    ) -> Self {
        let entries: Vec<AuthMethodEntry> = auth_methods
            .iter()
            .filter(|method| AuthMethodKind::from_id(method.id()).needs_interactive_login())
            .map(|method| AuthMethodEntry::from_auth_method(method, current_method_id))
            .collect();
        let initial = initial_selection(&entries, preferred_method_id, current_method_id);
        let mut state = Self {
            entries,
            selected: 0,
            switch_account,
            picker: PickerState::default(),
            window: ModalWindowState::new(),
        };
        state.set_selected(initial);
        state
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn selected_entry(&self) -> Option<&AuthMethodEntry> {
        self.entries.get(self.selected)
    }

    /// The method id under the cursor, if any.
    pub fn selected_method_id(&self) -> Option<acp::AuthMethodId> {
        self.selected_entry().map(|entry| entry.method_id.clone())
    }

    /// Clamp and apply a selection. The single place selection is written, so
    /// the renderer's mirror can never drift out of range.
    pub fn set_selected(&mut self, idx: usize) {
        self.selected = match self.entries.len() {
            0 => 0,
            len => idx.min(len - 1),
        };
        self.picker.selected = self.selected;
    }

    pub fn select_next(&mut self) {
        if self.entries.is_empty() {
            return;
        }
        let next = (self.selected + 1) % self.entries.len();
        self.set_selected(next);
    }

    pub fn select_prev(&mut self) {
        if self.entries.is_empty() {
            return;
        }
        let prev = if self.selected == 0 {
            self.entries.len() - 1
        } else {
            self.selected - 1
        };
        self.set_selected(prev);
    }
}

/// Selection order: the explicitly preferred method, else the method actually
/// in use, else the first entry. No kind is preferred over another.
fn initial_selection(
    entries: &[AuthMethodEntry],
    preferred_method_id: Option<&acp::AuthMethodId>,
    current_method_id: Option<&acp::AuthMethodId>,
) -> usize {
    let position = |wanted: Option<&acp::AuthMethodId>| {
        wanted.and_then(|id| entries.iter().position(|entry| &entry.method_id == id))
    };
    position(preferred_method_id)
        .or_else(|| position(current_method_id))
        .unwrap_or(0)
}

fn auth_kind_label(kind: AuthMethodKind) -> &'static str {
    match kind {
        AuthMethodKind::XaiApiKey => "API key",
        AuthMethodKind::CachedToken => "Cached session",
        AuthMethodKind::GrokCom => "xAI login",
        AuthMethodKind::Oidc => "OIDC",
        AuthMethodKind::PluginOauth => "Plugin OAuth",
        AuthMethodKind::Unknown => "Unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn method(id: &str, name: &str) -> acp::AuthMethod {
        acp::AuthMethod::Agent(acp::AuthMethodAgent::new(
            acp::AuthMethodId::new(id),
            name.to_string(),
        ))
    }

    /// Fixture with a deliberate mix: only two of the four methods are
    /// interactive, so a broken filter changes the result.
    fn mixed_methods() -> Vec<acp::AuthMethod> {
        vec![
            method("xai.api_key", "API key"),
            method("grok.com", "xAI"),
            method("cached_token", "Cached session"),
            method("plugin-oauth:example-auth", "Acme OAuth"),
        ]
    }

    fn ids(state: &AuthMethodPickerState) -> Vec<String> {
        state
            .entries
            .iter()
            .map(|entry| entry.method_id.0.to_string())
            .collect()
    }

    #[test]
    fn drops_non_interactive_methods() {
        let state = AuthMethodPickerState::new(&mixed_methods(), None, None, false);
        assert_eq!(ids(&state), vec!["grok.com", "plugin-oauth:example-auth"]);
    }

    #[test]
    fn defaults_to_first_entry_without_a_preference() {
        let state = AuthMethodPickerState::new(&mixed_methods(), None, None, false);
        assert_eq!(
            state.selected_method_id().map(|id| id.0.to_string()),
            Some("grok.com".to_string()),
            "no kind is preferred; the first advertised entry wins"
        );
    }

    #[test]
    fn preference_beats_first_entry() {
        let preferred = acp::AuthMethodId::new("plugin-oauth:example-auth");
        let state = AuthMethodPickerState::new(&mixed_methods(), None, Some(&preferred), false);
        assert_eq!(
            state.selected_method_id().map(|id| id.0.to_string()),
            Some("plugin-oauth:example-auth".to_string())
        );
    }

    #[test]
    fn current_method_selected_when_no_preference() {
        let current = acp::AuthMethodId::new("plugin-oauth:example-auth");
        let state = AuthMethodPickerState::new(&mixed_methods(), Some(&current), None, false);
        assert_eq!(
            state.selected_method_id().map(|id| id.0.to_string()),
            Some("plugin-oauth:example-auth".to_string())
        );
    }

    #[test]
    fn preference_pointing_at_a_filtered_method_falls_back() {
        let preferred = acp::AuthMethodId::new("xai.api_key");
        let state = AuthMethodPickerState::new(&mixed_methods(), None, Some(&preferred), false);
        assert_eq!(
            state.selected_method_id().map(|id| id.0.to_string()),
            Some("grok.com".to_string())
        );
    }

    #[test]
    fn no_entry_is_badged_current_without_a_known_credential() {
        let state = AuthMethodPickerState::new(&mixed_methods(), None, None, false);
        assert!(state.entries.iter().all(|entry| entry.badge().is_empty()));
    }

    #[test]
    fn only_the_in_use_credential_is_badged_current() {
        let current = acp::AuthMethodId::new("grok.com");
        let state = AuthMethodPickerState::new(&mixed_methods(), Some(&current), None, false);
        let badges: Vec<&str> = state.entries.iter().map(|entry| entry.badge()).collect();
        assert_eq!(badges, vec!["current", ""]);
    }

    #[test]
    fn two_accounts_on_one_provider_stay_separate_rows() {
        let methods = vec![
            method("plugin-oauth:example-auth", "Acme OAuth"),
            method("plugin-oauth:example-auth#2", "Acme OAuth"),
        ];
        let state = AuthMethodPickerState::new(&methods, None, None, false);
        assert_eq!(state.len(), 2, "entries must not be deduped by provider");
        assert_eq!(
            ids(&state),
            vec!["plugin-oauth:example-auth", "plugin-oauth:example-auth#2"]
        );
    }

    /// The agent advertises one method per account, each carrying the provider
    /// and account labels in `meta`. The picker must render two rows that share
    /// a provider name yet stay tellable apart by their account column.
    #[test]
    fn accounts_advertised_by_a_plugin_render_as_distinguishable_rows() {
        use xai_grok_shell::agent::auth_method::plugin_oauth_auth_method;

        let methods = vec![
            plugin_oauth_auth_method(
                "example-auth",
                "Acme OAuth",
                Some("work"),
                Some("work@example.com"),
            ),
            plugin_oauth_auth_method(
                "example-auth",
                "Acme OAuth",
                Some("personal"),
                Some("personal@example.com"),
            ),
        ];
        let state = AuthMethodPickerState::new(&methods, None, None, false);

        assert_eq!(state.len(), 2);
        assert_eq!(
            ids(&state),
            vec![
                "plugin-oauth:example-auth#work",
                "plugin-oauth:example-auth#personal"
            ]
        );
        assert!(
            state
                .entries
                .iter()
                .all(|entry| entry.provider_label == "Acme OAuth"),
            "both rows name the same provider"
        );
        assert_eq!(
            state
                .entries
                .iter()
                .map(|entry| entry.right_label())
                .collect::<Vec<_>>(),
            vec!["work@example.com", "personal@example.com"],
            "the account column is what tells the two rows apart"
        );
        // The account is also spelled out in the detail rows.
        assert!(
            state.entries[0]
                .details
                .iter()
                .any(|d| d.label == "Account" && d.value == "work@example.com")
        );
    }

    /// A method without account meta keeps today's shape: the method name is
    /// the provider label and nothing lands in the account column.
    #[test]
    fn account_less_method_has_no_account_label() {
        use xai_grok_shell::agent::auth_method::plugin_oauth_auth_method;

        let methods = vec![plugin_oauth_auth_method(
            "example-auth",
            "Acme OAuth",
            None,
            None,
        )];
        let state = AuthMethodPickerState::new(&methods, None, None, false);
        assert_eq!(state.entries[0].provider_label, "Acme OAuth");
        assert_eq!(state.entries[0].account_label, None);
        assert!(state.entries[0].right_label().is_empty());
        assert!(
            !state.entries[0]
                .details
                .iter()
                .any(|d| d.label == "Account")
        );
    }

    /// A method with no `meta` at all (every built-in one) still labels itself
    /// from the method name.
    #[test]
    fn provider_label_falls_back_to_the_method_name_without_meta() {
        let state = AuthMethodPickerState::new(&mixed_methods(), None, None, false);
        assert_eq!(state.entries[0].provider_label, "xAI");
        assert_eq!(state.entries[0].account_label, None);
    }

    #[test]
    fn right_label_distinguishes_accounts_on_one_provider() {
        let mut state = AuthMethodPickerState::new(
            &[
                method("plugin-oauth:example-auth", "Acme OAuth"),
                method("plugin-oauth:example-auth#2", "Acme OAuth"),
            ],
            None,
            None,
            false,
        );
        assert!(
            state
                .entries
                .iter()
                .all(|entry| entry.right_label().is_empty()),
            "no account labels yet: emit nothing rather than a constant label"
        );
        state.entries[0].account_label = Some("first@example.com".to_string());
        state.entries[1].account_label = Some("second@example.com".to_string());
        assert_eq!(state.entries[0].right_label(), "first@example.com");
        assert_eq!(state.entries[1].right_label(), "second@example.com");
    }

    #[test]
    fn navigation_wraps_and_mirrors_into_the_renderer_state() {
        let mut state = AuthMethodPickerState::new(&mixed_methods(), None, None, false);
        state.select_prev();
        assert_eq!(state.selected, 1);
        assert_eq!(state.picker.selected, 1);
        state.select_next();
        assert_eq!(state.selected, 0);
        assert_eq!(state.picker.selected, 0);
    }

    #[test]
    fn set_selected_clamps_out_of_range() {
        let mut state = AuthMethodPickerState::new(&mixed_methods(), None, None, false);
        state.set_selected(99);
        assert_eq!(state.selected, state.len() - 1);
        assert_eq!(state.picker.selected, state.selected);
    }

    #[test]
    fn empty_list_has_no_selection() {
        let state =
            AuthMethodPickerState::new(&[method("xai.api_key", "API key")], None, None, true);
        assert!(state.is_empty());
        assert_eq!(state.selected_method_id(), None);
        assert!(state.switch_account, "switch_account is carried verbatim");
    }
}
