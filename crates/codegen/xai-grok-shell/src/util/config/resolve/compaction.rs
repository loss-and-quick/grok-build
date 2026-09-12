/// Default auto-compact threshold (% of context window) when no source sets it.
pub const DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT: u8 = 85;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CompactionToolChoice {
    #[default]
    Auto,
    None,
}

impl std::str::FromStr for CompactionToolChoice {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "none" => Ok(Self::None),
            _ => Err(()),
        }
    }
}

pub(crate) const ENV_COMPACTION_TOOL_CHOICE: &str = "GROK_COMPACTION_TOOL_CHOICE";

pub(crate) fn resolve_compaction_tool_choice_from(
    env: Option<&str>,
    config: Option<&str>,
    remote: Option<&str>,
) -> CompactionToolChoice {
    env.and_then(|s| s.parse().ok())
        .or_else(|| config.and_then(|s| s.parse().ok()))
        .or_else(|| remote.and_then(|s| s.parse().ok()))
        .unwrap_or_default()
}

pub(crate) const ENV_AUTO_COMPACT_THRESHOLD_PERCENT: &str = "GROK_AUTO_COMPACT_THRESHOLD_PERCENT";

/// Precedence (highest first):
///   1. env `GROK_AUTO_COMPACT_THRESHOLD_PERCENT`
///   2. user TOML `[model.<id>].auto_compact_threshold_percent` (`cfg.config_models`, the merge of user and managed `[model.<id>]` sections)
///   3. user TOML `[session].auto_compact_threshold_percent`
///   4. remote settings per-model `ModelInfo.auto_compact_threshold_percent`
///      (kept out of `ConfigModelOverride::apply` so the user and remote per-model tiers stay distinct)
///   5. remote settings global `RemoteSettings.auto_compact_threshold_percent`
///   6. default `DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT`
pub(crate) fn resolve_auto_compact_threshold_percent(
    cfg: &crate::agent::config::Config,
    model_id: &str,
    model: Option<&crate::agent::config::ModelInfo>,
) -> u8 {
    resolve_auto_compact_threshold_percent_from_tiers(
        cfg.config_models
            .get(model_id)
            .and_then(|m| m.auto_compact_threshold_percent),
        cfg.session.auto_compact_threshold_percent,
        model.and_then(|m| m.auto_compact_threshold_percent),
        cfg.remote_settings
            .as_ref()
            .and_then(|r| r.auto_compact_threshold_percent),
    )
}

/// [`resolve_auto_compact_threshold_percent`] for callers without a `Config`, e.g. subagent spawn paths that pass the parent's tiers explicitly.
/// There the per-model tier uses the subagent's resolved model id, not the parent's.
pub(crate) fn resolve_auto_compact_threshold_percent_from_tiers(
    user_per_model: Option<u8>,
    user_global: Option<u8>,
    gb_per_model: Option<u8>,
    gb_global: Option<u8>,
) -> u8 {
    fn clamp_env(raw: i64) -> Option<u8> {
        if (0..=100).contains(&raw) {
            Some(raw as u8)
        } else {
            tracing::debug!(
                source = "env",
                value = raw,
                "auto_compact_threshold_percent out of range 0..=100; ignoring"
            );
            None
        }
    }
    let from_env = || -> Option<u8> {
        std::env::var(ENV_AUTO_COMPACT_THRESHOLD_PERCENT)
            .ok()
            .and_then(|s| s.parse::<i64>().ok())
            .and_then(clamp_env)
    };

    from_env()
        .or(user_per_model)
        .or(user_global)
        .or(gb_per_model)
        .or(gb_global)
        .unwrap_or(DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT)
}

/// Fleet p99 of successful compactions is ~181s (≈225s at 400K+ input).
/// So 300s clears the legit tail with margin while cutting a runaway from the ~600s deadline.
pub const DEFAULT_COMPACTION_WALL_CLOCK_BUDGET_SECS: u64 = 300;

/// Below this, a configured budget is almost certainly a misconfig (fleet success p99 ~181s); logged at `warn`, not clamped.
const COMPACTION_WALL_CLOCK_BUDGET_WARN_SECS: u64 = 120;

const ENV_COMPACTION_WALL_CLOCK_BUDGET_SECS: &str = "GROK_COMPACTION_WALL_CLOCK_SECS";

/// Precedence: env `GROK_COMPACTION_WALL_CLOCK_SECS`, then user TOML `[session].compaction_wall_clock_budget_secs`, then remote `RemoteSettings.compaction_wall_clock_budget_secs`, then the client default.
/// `0` **disables** it.
/// Low values are warned, not clamped: any "safe" clamp (e.g. 30s) would itself cut legit compactions, trading one silent failure for another.
/// Ops own the value.
pub(crate) fn resolve_compaction_wall_clock_budget_secs(
    user_session: Option<u64>,
    gb_global: Option<u64>,
) -> u64 {
    let from_env = std::env::var(ENV_COMPACTION_WALL_CLOCK_BUDGET_SECS)
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok());
    let resolved = from_env
        .or(user_session)
        .or(gb_global)
        .unwrap_or(DEFAULT_COMPACTION_WALL_CLOCK_BUDGET_SECS);
    if resolved > 0 && resolved < COMPACTION_WALL_CLOCK_BUDGET_WARN_SECS {
        tracing::warn!(
            budget_secs = resolved,
            "compaction wall-clock budget {resolved}s is below {COMPACTION_WALL_CLOCK_BUDGET_WARN_SECS}s \
             and may cut legitimate compactions (fleet success p99 ~181s); set 0 to disable"
        );
    }
    resolved
}

#[cfg(test)]
mod compaction_wall_clock_budget_tests {
    use super::{
        DEFAULT_COMPACTION_WALL_CLOCK_BUDGET_SECS, ENV_COMPACTION_WALL_CLOCK_BUDGET_SECS,
        resolve_compaction_wall_clock_budget_secs as resolve,
    };
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvVarGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        prev: Option<String>,
    }

    impl EnvVarGuard {
        fn set(value: &str) -> Self {
            let lock = ENV_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let prev = std::env::var(ENV_COMPACTION_WALL_CLOCK_BUDGET_SECS).ok();
            unsafe { std::env::set_var(ENV_COMPACTION_WALL_CLOCK_BUDGET_SECS, value) };
            Self { _lock: lock, prev }
        }

        fn unset() -> Self {
            let lock = ENV_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let prev = std::env::var(ENV_COMPACTION_WALL_CLOCK_BUDGET_SECS).ok();
            unsafe { std::env::remove_var(ENV_COMPACTION_WALL_CLOCK_BUDGET_SECS) };
            Self { _lock: lock, prev }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match self.prev.take() {
                Some(v) => unsafe { std::env::set_var(ENV_COMPACTION_WALL_CLOCK_BUDGET_SECS, v) },
                None => unsafe { std::env::remove_var(ENV_COMPACTION_WALL_CLOCK_BUDGET_SECS) },
            }
        }
    }

    #[test]
    fn defaults_to_client_budget_when_all_sources_unset() {
        let _g = EnvVarGuard::unset();
        assert_eq!(
            resolve(None, None),
            DEFAULT_COMPACTION_WALL_CLOCK_BUDGET_SECS
        );
    }

    #[test]
    fn user_session_beats_remote() {
        let _g = EnvVarGuard::unset();
        assert_eq!(resolve(Some(450), Some(300)), 450);
    }

    #[test]
    fn remote_beats_default() {
        let _g = EnvVarGuard::unset();
        assert_eq!(resolve(None, Some(450)), 450);
    }

    #[test]
    fn zero_disables_budget_from_user_config() {
        let _g = EnvVarGuard::unset();
        assert_eq!(resolve(Some(0), Some(450)), 0);
    }

    #[test]
    fn low_values_are_not_clamped() {
        let _g = EnvVarGuard::unset();
        assert_eq!(resolve(Some(5), Some(450)), 5);
    }

    #[test]
    fn env_beats_user_config_and_remote() {
        let _g = EnvVarGuard::set("900");
        assert_eq!(resolve(Some(450), Some(300)), 900);
    }

    #[test]
    fn invalid_env_falls_through_to_user_config() {
        let _g = EnvVarGuard::set("not-a-number");
        assert_eq!(resolve(Some(450), Some(300)), 450);
    }
}

#[cfg(test)]
mod compaction_tool_choice_tests {
    use super::{CompactionToolChoice, resolve_compaction_tool_choice_from as resolve};

    #[test]
    fn default_is_auto() {
        assert_eq!(resolve(None, None, None), CompactionToolChoice::Auto);
    }

    #[test]
    fn precedence_env_over_config_over_remote() {
        assert_eq!(
            resolve(Some("none"), Some("auto"), Some("auto")),
            CompactionToolChoice::None
        );
        assert_eq!(
            resolve(None, Some("none"), Some("auto")),
            CompactionToolChoice::None
        );
        assert_eq!(
            resolve(None, None, Some("none")),
            CompactionToolChoice::None
        );
    }

    #[test]
    fn garbage_falls_through() {
        assert_eq!(
            resolve(Some("garbage"), None, Some("none")),
            CompactionToolChoice::None
        );
        assert_eq!(
            resolve(Some("garbage"), Some("also-bad"), None),
            CompactionToolChoice::Auto
        );
    }

    #[test]
    fn from_str_case_insensitive() {
        assert_eq!("AUTO".parse(), Ok(CompactionToolChoice::Auto));
        assert_eq!(" None ".parse(), Ok(CompactionToolChoice::None));
        assert!("required".parse::<CompactionToolChoice>().is_err());
    }
}
