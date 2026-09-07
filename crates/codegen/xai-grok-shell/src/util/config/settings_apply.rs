//! Writing one registered setting, by key.
//!
//! This is the only table that maps a settings key onto a write, and it lives
//! here rather than in a client because a client that owned it would be a
//! client the others had to copy. The pager reaches it through
//! `Effect::PersistSetting`; every other client reaches it through
//! `x.ai/settings/set`. Both therefore inherit the same refusal on a
//! `config.toml` grok was not given to rewrite: every arm goes through
//! [`super::persist::update_config`], which asks [`super::readonly`] first.
//!
//! The values arrive in their wire form ([`SettingValue`]) rather than in a
//! client's own value type, so nothing in this path is expressible only in
//! Rust that runs inside the pager.

use xai_grok_settings_types::{SettingValue, is_plugin_key, split_plugin_key};

/// What became of a write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingWrite {
    /// The value reached `config.toml`.
    Persisted,
    /// The row's value is not the shell's to hold; the client that rendered it
    /// keeps it. Not a failure, and not something to report as one.
    ClientOwned,
}

/// Write one setting's value.
///
/// A type mismatch and an unknown key come back as errors rather than panics:
/// this runs in a spawned task on the pager's side and in a request handler on
/// every other client's, and neither should be able to take the process down by
/// sending the wrong shape.
pub async fn persist_setting(key: &str, value: SettingValue) -> Result<SettingWrite, String> {
    // The one registered setting whose value does not live in `config.toml`:
    // the pager keeps it in its own `pager.toml`, which is a client's file.
    if key == "respect_manual_folds" {
        return Ok(SettingWrite::ClientOwned);
    }
    write_to_config(key, value)
        .await
        .map(|()| SettingWrite::Persisted)
}

async fn write_to_config(key: &str, value: SettingValue) -> Result<(), String> {
    fn kind_mismatch(key: &str, expected: &str, got: &SettingValue) -> String {
        format!("persist_setting({key}) expected {expected}, got {got:?}")
    }
    match key {
        "compact_mode" => {
            let SettingValue::Bool(b) = value else {
                return Err(kind_mismatch("compact_mode", "Bool", &value));
            };
            crate::util::config::set_compact_mode(b)
                .await
                .map_err(|e| e.to_string())
        }
        "trace_upload" => {
            let SettingValue::Bool(b) = value else {
                return Err(kind_mismatch("trace_upload", "Bool", &value));
            };
            crate::util::config::set_trace_upload(b)
                .await
                .map_err(|e| e.to_string())
        }
        "feedback_trace_card" => {
            let SettingValue::Bool(b) = value else {
                return Err(kind_mismatch("feedback_trace_card", "Bool", &value));
            };
            crate::util::config::set_feedback_trace_card(b)
                .await
                .map_err(|e| e.to_string())
        }
        "show_timestamps" => {
            let SettingValue::Bool(b) = value else {
                return Err(kind_mismatch("show_timestamps", "Bool", &value));
            };
            crate::util::config::set_show_timestamps(b)
                .await
                .map_err(|e| e.to_string())
        }
        "page_flip_on_send" => {
            let SettingValue::Bool(b) = value else {
                return Err(kind_mismatch("page_flip_on_send", "Bool", &value));
            };
            crate::util::config::set_page_flip_on_send(b)
                .await
                .map_err(|e| e.to_string())
        }
        "confirm_before_rewind" => {
            let SettingValue::Bool(b) = value else {
                return Err(kind_mismatch("confirm_before_rewind", "Bool", &value));
            };
            crate::util::config::set_confirm_before_rewind(b)
                .await
                .map_err(|e| e.to_string())
        }
        "combine_queued_prompts" => {
            let SettingValue::Bool(b) = value else {
                return Err(kind_mismatch("combine_queued_prompts", "Bool", &value));
            };
            crate::util::config::set_combine_queued_prompts(b)
                .await
                .map_err(|e| e.to_string())
        }
        "follow_up_behavior" => {
            let SettingValue::String(s) = value else {
                return Err(kind_mismatch("follow_up_behavior", "String", &value));
            };
            crate::util::config::set_follow_up_behavior(s)
                .await
                .map_err(|e| e.to_string())
        }
        "show_timeline" => {
            let SettingValue::Bool(b) = value else {
                return Err(kind_mismatch("show_timeline", "Bool", &value));
            };
            crate::util::config::set_show_timeline(b)
                .await
                .map_err(|e| e.to_string())
        }
        "simple_mode" => {
            let SettingValue::Bool(b) = value else {
                return Err(kind_mismatch("simple_mode", "Bool", &value));
            };
            crate::util::config::set_simple_mode(b)
                .await
                .map_err(|e| e.to_string())
        }
        "contextual_hints.undo" => {
            let SettingValue::Bool(b) = value else {
                return Err(kind_mismatch("contextual_hints.undo", "Bool", &value));
            };
            crate::util::config::set_contextual_hint_undo(b)
                .await
                .map_err(|e| e.to_string())
        }
        "contextual_hints.plan_mode" => {
            let SettingValue::Bool(b) = value else {
                return Err(kind_mismatch("contextual_hints.plan_mode", "Bool", &value));
            };
            crate::util::config::set_contextual_hint_plan_mode(b)
                .await
                .map_err(|e| e.to_string())
        }
        "contextual_hints.image_input" => {
            let SettingValue::Bool(b) = value else {
                return Err(kind_mismatch(
                    "contextual_hints.image_input",
                    "Bool",
                    &value,
                ));
            };
            crate::util::config::set_contextual_hint_image_input(b)
                .await
                .map_err(|e| e.to_string())
        }
        "contextual_hints.send_now" => {
            let SettingValue::Bool(b) = value else {
                return Err(kind_mismatch("contextual_hints.send_now", "Bool", &value));
            };
            crate::util::config::set_contextual_hint_send_now(b)
                .await
                .map_err(|e| e.to_string())
        }
        "contextual_hints.small_screen" => {
            let SettingValue::Bool(b) = value else {
                return Err(kind_mismatch(
                    "contextual_hints.small_screen",
                    "Bool",
                    &value,
                ));
            };
            crate::util::config::set_contextual_hint_small_screen(b)
                .await
                .map_err(|e| e.to_string())
        }
        "contextual_hints.word_select" => {
            let SettingValue::Bool(b) = value else {
                return Err(kind_mismatch(
                    "contextual_hints.word_select",
                    "Bool",
                    &value,
                ));
            };
            crate::util::config::set_contextual_hint_word_select(b)
                .await
                .map_err(|e| e.to_string())
        }
        "contextual_hints.export_copy" => {
            let SettingValue::Bool(b) = value else {
                return Err(kind_mismatch(
                    "contextual_hints.export_copy",
                    "Bool",
                    &value,
                ));
            };
            crate::util::config::set_contextual_hint_export_copy(b)
                .await
                .map_err(|e| e.to_string())
        }
        "contextual_hints.ssh_wrap" => {
            let SettingValue::Bool(b) = value else {
                return Err(kind_mismatch("contextual_hints.ssh_wrap", "Bool", &value));
            };
            crate::util::config::set_contextual_hint_ssh_wrap(b)
                .await
                .map_err(|e| e.to_string())
        }
        "theme" => {
            let SettingValue::String(s) = value else {
                return Err(kind_mismatch("theme", "String", &value));
            };
            crate::util::config::set_theme(s)
                .await
                .map_err(|e| e.to_string())
        }
        "auto_dark_theme" => {
            let SettingValue::String(s) = value else {
                return Err(kind_mismatch("auto_dark_theme", "String", &value));
            };
            crate::util::config::set_auto_dark_theme(s)
                .await
                .map_err(|e| e.to_string())
        }
        "auto_light_theme" => {
            let SettingValue::String(s) = value else {
                return Err(kind_mismatch("auto_light_theme", "String", &value));
            };
            crate::util::config::set_auto_light_theme(s)
                .await
                .map_err(|e| e.to_string())
        }
        "default_model" => {
            let SettingValue::String(s) = value else {
                return Err(kind_mismatch("default_model", "String", &value));
            };
            crate::util::config::set_default_model(s)
                .await
                .map_err(|e| e.to_string())
        }
        "scroll_speed" => {
            let SettingValue::Int(i) = value else {
                return Err(kind_mismatch("scroll_speed", "Int", &value));
            };
            crate::util::config::set_scroll_speed(i)
                .await
                .map_err(|e| e.to_string())
        }
        "scroll_mode" => {
            let SettingValue::String(s) = value else {
                return Err(kind_mismatch("scroll_mode", "String", &value));
            };
            crate::util::config::set_scroll_mode(s)
                .await
                .map_err(|e| e.to_string())
        }
        "invert_scroll" => {
            let SettingValue::Bool(b) = value else {
                return Err(kind_mismatch("invert_scroll", "Bool", &value));
            };
            crate::util::config::set_invert_scroll(b)
                .await
                .map_err(|e| e.to_string())
        }
        "display_refresh_auto_cadence" => {
            let SettingValue::Bool(b) = value else {
                return Err(kind_mismatch(
                    "display_refresh_auto_cadence",
                    "Bool",
                    &value,
                ));
            };
            crate::util::config::set_display_refresh_auto_cadence(b)
                .await
                .map_err(|e| e.to_string())
        }
        "scroll_lines" => {
            let SettingValue::Int(i) = value else {
                return Err(kind_mismatch("scroll_lines", "Int", &value));
            };
            crate::util::config::set_scroll_lines(i)
                .await
                .map_err(|e| e.to_string())
        }
        "default_selected_permission" => {
            let SettingValue::String(s) = value else {
                return Err(kind_mismatch(
                    "default_selected_permission",
                    "String",
                    &value,
                ));
            };
            crate::util::config::set_default_selected_permission(s)
                .await
                .map_err(|e| e.to_string())
        }
        "cancel_subagents_on_turn_cancel" => {
            let SettingValue::String(s) = value else {
                return Err(kind_mismatch(
                    "cancel_subagents_on_turn_cancel",
                    "String",
                    &value,
                ));
            };
            crate::util::config::set_cancel_subagents_on_turn_cancel(s)
                .await
                .map_err(|e| e.to_string())
        }
        "vim_mode" => {
            let SettingValue::Bool(b) = value else {
                return Err(kind_mismatch("vim_mode", "Bool", &value));
            };
            crate::util::config::set_vim_mode(b)
                .await
                .map_err(|e| e.to_string())
        }
        "remember_tool_approvals" => {
            let SettingValue::Bool(b) = value else {
                return Err(kind_mismatch("remember_tool_approvals", "Bool", &value));
            };
            crate::util::config::set_remember_tool_approvals(b)
                .await
                .map_err(|e| e.to_string())
        }
        "toolset.ask_user_question.timeout_enabled" => {
            let SettingValue::Bool(b) = value else {
                return Err(kind_mismatch(
                    "toolset.ask_user_question.timeout_enabled",
                    "Bool",
                    &value,
                ));
            };
            crate::util::config::set_ask_user_question_timeout_enabled(b)
                .await
                .map_err(|e| e.to_string())
        }
        "show_thinking_blocks" => {
            let SettingValue::Bool(b) = value else {
                return Err(kind_mismatch("show_thinking_blocks", "Bool", &value));
            };
            crate::util::config::set_show_thinking_blocks(b)
                .await
                .map_err(|e| e.to_string())
        }
        "group_tool_verbs" => {
            let SettingValue::Bool(b) = value else {
                return Err(kind_mismatch("group_tool_verbs", "Bool", &value));
            };
            crate::util::config::set_group_tool_verbs(b)
                .await
                .map_err(|e| e.to_string())
        }
        "collapsed_edit_blocks" => {
            let SettingValue::Bool(b) = value else {
                return Err(kind_mismatch("collapsed_edit_blocks", "Bool", &value));
            };
            crate::util::config::set_collapsed_edit_blocks(b)
                .await
                .map_err(|e| e.to_string())
        }
        "prompt_suggestions" => {
            let SettingValue::Bool(b) = value else {
                return Err(kind_mismatch("prompt_suggestions", "Bool", &value));
            };
            crate::util::config::set_prompt_suggestions(b)
                .await
                .map_err(|e| e.to_string())
        }
        "keep_text_selection" => {
            let SettingValue::String(s) = value else {
                return Err(kind_mismatch("keep_text_selection", "String", &value));
            };
            crate::util::config::set_keep_text_selection(s)
                .await
                .map_err(|e| e.to_string())
        }
        "render_mermaid" => {
            let SettingValue::String(s) = value else {
                return Err(kind_mismatch("render_mermaid", "String", &value));
            };
            crate::util::config::set_render_mermaid(s)
                .await
                .map_err(|e| e.to_string())
        }
        "hunk_tracker_mode" => {
            let SettingValue::String(s) = value else {
                return Err(kind_mismatch("hunk_tracker_mode", "String", &value));
            };
            crate::util::config::set_hunk_tracker_mode(s)
                .await
                .map_err(|e| e.to_string())
        }
        "screen_mode" => {
            let SettingValue::String(s) = value else {
                return Err(kind_mismatch("screen_mode", "String", &value));
            };
            crate::util::config::set_screen_mode(s)
                .await
                .map_err(|e| e.to_string())
        }
        "voice_keybind_enabled" => {
            let SettingValue::Bool(b) = value else {
                return Err(kind_mismatch("voice_keybind_enabled", "Bool", &value));
            };
            crate::util::config::set_voice_keybind_enabled(b)
                .await
                .map_err(|e| e.to_string())
        }
        "voice_capture_mode" => {
            let SettingValue::String(s) = value else {
                return Err(kind_mismatch("voice_capture_mode", "String", &value));
            };
            crate::util::config::set_voice_capture_mode(s)
                .await
                .map_err(|e| e.to_string())
        }
        "voice_stt_language" => {
            let SettingValue::String(s) = value else {
                return Err(kind_mismatch("voice_stt_language", "String", &value));
            };
            crate::util::config::set_voice_stt_language(s)
                .await
                .map_err(|e| e.to_string())
        }
        "max_thoughts_width" => {
            let SettingValue::Int(i) = value else {
                return Err(kind_mismatch("max_thoughts_width", "Int", &value));
            };
            crate::util::config::set_max_thoughts_width(i)
                .await
                .map_err(|e| e.to_string())
        }
        "show_tips" => {
            let SettingValue::Bool(b) = value else {
                return Err(kind_mismatch("show_tips", "Bool", &value));
            };
            crate::util::config::set_show_tips(b)
                .await
                .map_err(|e| e.to_string())
        }
        "auto_update" => {
            let SettingValue::Bool(b) = value else {
                return Err(kind_mismatch("auto_update", "Bool", &value));
            };
            crate::util::config::set_auto_update(b)
                .await
                .map_err(|e| e.to_string())
        }
        "fork_secondary_model" => {
            let SettingValue::String(s) = value else {
                return Err(kind_mismatch("fork_secondary_model", "String", &value));
            };
            crate::util::config::set_fork_secondary_model(s)
                .await
                .map_err(|e| e.to_string())
        }
        // Plugin-contributed rows: `plugin.<plugin>.<setting>` writes
        // `[plugins.<plugin>].<setting>`. One arm rather than a helper per row,
        // because the rows are discovered from plugin manifests at runtime and
        // there is no compile-time set of them to write helpers for. The write
        // still lands in `update_config`, which is what refuses it when the
        // user's `config.toml` is not grok's to rewrite.
        key if is_plugin_key(key) => {
            let Some((plugin, setting)) = split_plugin_key(key) else {
                return Err(format!("malformed plugin setting key: `{key}`"));
            };
            crate::util::config::set_plugin_setting(
                plugin.to_string(),
                setting.to_string(),
                plugin_json_value(&value),
            )
            .await
            .map_err(|e| e.to_string())
        }
        other => Err(format!("unknown setting key for persist: `{other}`")),
    }
}

/// A plugin row's value as JSON, which is what `[plugins.<name>]` stores.
fn plugin_json_value(value: &SettingValue) -> serde_json::Value {
    match value {
        SettingValue::Bool(b) => serde_json::Value::Bool(*b),
        SettingValue::Int(i) => serde_json::Value::from(*i),
        SettingValue::String(s) => serde_json::Value::String(s.clone()),
    }
}
