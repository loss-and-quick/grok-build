//! `x.ai/plugins/*` extension handlers, backing the pager's hooks/plugins modal.

use agent_client_protocol as acp;
use serde::Deserialize;
use xai_hooks_plugins_types::{
    HookStatus, McpStatus, PluginInfo, PluginOrigin, PluginScope, PluginSettingChoiceInfo,
    PluginSettingInfo, PluginSettingKindInfo, PluginsListResponse,
};

use crate::agent::MvpAgent;

type ExtResult = Result<acp::ExtResponse, acp::Error>;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListRequest {
    session_id: String,
}

pub(crate) fn loaded_plugin_to_info(plugin: &xai_grok_agent::plugins::LoadedPlugin) -> PluginInfo {
    use xai_grok_agent::plugins::discovery::PluginScope as AgentScope;

    let scope = match plugin.scope {
        AgentScope::CliOverride => PluginScope::Cli,
        AgentScope::Project => PluginScope::Project,
        AgentScope::User => PluginScope::User,
        AgentScope::ConfigPath => PluginScope::Config,
    };

    let origin = origin_to_dto(&plugin.origin);

    let hook_status = if !plugin.has_hooks {
        HookStatus::None
    } else if !plugin.trusted {
        HookStatus::Blocked
    } else if plugin.has_inline_hooks_only {
        HookStatus::ActiveInline
    } else {
        HookStatus::Active
    };

    let mcp_status = if plugin.mcp_server_count == 0 {
        McpStatus::None
    } else if !plugin.trusted {
        McpStatus::Blocked
    } else if plugin.has_inline_mcp_only {
        McpStatus::ActiveInline
    } else {
        McpStatus::Active
    };

    PluginInfo {
        name: plugin.name.clone(),
        id: plugin.id.0.clone(),
        root: plugin.root.display().to_string(),
        scope,
        trusted: plugin.trusted,
        enabled: plugin.enabled,
        version: plugin.version.clone(),
        description: plugin.description.clone(),
        skill_count: plugin.skill_count,
        skill_names: plugin.skill_names.clone(),
        agent_count: plugin.agent_count,
        agent_names: plugin.agent_names.clone(),
        hook_status,
        hook_count: plugin.hook_count,
        mcp_server_count: plugin.mcp_server_count,
        mcp_status,
        marketplace_source: marketplace_source_label(&origin),
        origin: Some(origin),
        conflict: plugin.conflict.clone(),
        load_error: plugin.load_error.clone(),
        // A disabled plugin contributes no rows: `[plugins].disabled` already
        // means "this plugin is not part of the session", and a settings row
        // for it would offer to configure something that is not running.
        // Untrusted plugins are already filtered upstream (`LoadedPlugin::settings`).
        settings: if plugin.enabled {
            plugin.settings.iter().map(setting_to_info).collect()
        } else {
            Vec::new()
        },
    }
}

/// Map one validated manifest setting to its wire DTO.
fn setting_to_info(spec: &xai_grok_agent::plugins::PluginSettingSpec) -> PluginSettingInfo {
    use xai_grok_agent::plugins::PluginSettingKind;
    let kind = match &spec.kind {
        PluginSettingKind::Bool { default } => PluginSettingKindInfo::Bool { default: *default },
        PluginSettingKind::String { default } => PluginSettingKindInfo::String {
            default: default.clone(),
        },
        PluginSettingKind::Int { default, min, max } => PluginSettingKindInfo::Int {
            default: *default,
            min: *min,
            max: *max,
        },
        PluginSettingKind::Enum { default, choices } => PluginSettingKindInfo::Enum {
            default: default.clone(),
            choices: choices
                .iter()
                .map(|c| PluginSettingChoiceInfo {
                    value: c.value.clone(),
                    label: c.label.clone(),
                    description: c.description.clone(),
                })
                .collect(),
        },
    };
    PluginSettingInfo {
        key: spec.key.clone(),
        label: spec.label.clone(),
        description: spec.description.clone(),
        kind,
    }
}

fn origin_to_dto(origin: &xai_grok_agent::plugins::PluginOrigin) -> PluginOrigin {
    use xai_grok_agent::plugins::PluginOrigin as AgentOrigin;
    match origin {
        AgentOrigin::CliOverride => PluginOrigin::CliOverride,
        AgentOrigin::ProjectGrok => PluginOrigin::ProjectGrok,
        AgentOrigin::ProjectClaude => PluginOrigin::ProjectClaude,
        AgentOrigin::UserGrok => PluginOrigin::UserGrok,
        AgentOrigin::UserClaude => PluginOrigin::UserClaude,
        AgentOrigin::ClaudeMarketplace { marketplace } => PluginOrigin::ClaudeMarketplace {
            marketplace: marketplace.clone(),
        },
        AgentOrigin::ClaudeInstalled { marketplace } => PluginOrigin::ClaudeInstalled {
            marketplace: marketplace.clone(),
        },
        AgentOrigin::MarketplaceInstall {
            source_name,
            git_url,
        } => PluginOrigin::MarketplaceInstall {
            source_name: source_name.clone(),
            git_url: git_url.clone(),
        },
        AgentOrigin::ConfigPath => PluginOrigin::ConfigPath,
    }
}

/// Derive the legacy `marketplace_source` label kept for older pagers: the marketplace display name, or `git: owner/repo` for a direct git install.
fn marketplace_source_label(origin: &PluginOrigin) -> Option<String> {
    match origin {
        PluginOrigin::MarketplaceInstall {
            source_name: Some(name),
            ..
        } => Some(name.clone()),
        PluginOrigin::MarketplaceInstall {
            source_name: None,
            git_url: Some(url),
        } => {
            // Shorten the URL: "https://github.com/obra/superpowers.git" becomes "obra/superpowers"
            let label = url
                .trim_end_matches(".git")
                .rsplit("://")
                .next()
                .and_then(|s| {
                    s.strip_prefix("github.com/")
                        .or_else(|| s.strip_prefix("gitlab.com/"))
                        .or(Some(s))
                })
                .unwrap_or(url);
            Some(format!("git: {label}"))
        }
        _ => None,
    }
}

pub async fn handle(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    match args.method.as_ref() {
        "x.ai/plugins/list" => {
            let req: ListRequest = super::parse_params(args)?;

            // A known session answers from its own registry, which includes `_meta.pluginDirs` plugins
            // Only an unknown session (a pull before any session exists) falls back to the shared snapshot
            let sid = acp::SessionId::new(req.session_id);
            let registry = match agent.session_handle_waiting_for_load(&sid).await {
                Some(handle) => handle.plugins_list().await,
                None => agent.plugin_registry_snapshot(),
            };
            let response = match registry {
                Some(registry) => {
                    let plugins = registry
                        .list()
                        .iter()
                        .map(|p| loaded_plugin_to_info(p))
                        .collect();
                    PluginsListResponse { plugins }
                }
                None => PluginsListResponse {
                    plugins: Vec::new(),
                },
            };
            super::to_ext_response(Ok::<_, anyhow::Error>(response))
        }
        "x.ai/plugins/action" => {
            let req: xai_hooks_plugins_types::PluginsActionRequest = super::parse_params(args)?;
            let sid = acp::SessionId::new(req.session_id);

            let result = agent
                .execute_plugins_action(&sid, req.action)
                .await
                .ok_or_else(|| anyhow::anyhow!("session not found"));
            super::to_ext_response(result)
        }
        "x.ai/plugins/panel_action" => {
            let req: xai_hooks_plugins_types::PanelActionRequest = super::parse_params(args)?;
            let sid = acp::SessionId::new(req.session_id);
            let delivered = agent
                .deliver_panel_action(&sid, req.plugin, req.panel_id, req.button_id, req.inputs)
                .await;
            super::to_ext_response(Ok::<_, anyhow::Error>(
                xai_hooks_plugins_types::PanelActionResponse { delivered },
            ))
        }
        "x.ai/plugins/notify-updates" => {
            // Broadcast a PluginUpdatesInstalled notification to the session.
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct NotifyUpdatesRequest {
                session_id: String,
                updates: Vec<(String, String, String)>, // (name, old_ver, new_ver)
            }
            let req: NotifyUpdatesRequest = super::parse_params(args)?;
            let sid = acp::SessionId::new(req.session_id);
            if let Some(handle) = agent.get_session_handle(&sid) {
                handle.notify_plugin_updates(req.updates).await;
            }
            super::to_ext_response(Ok::<_, anyhow::Error>(serde_json::json!({ "ok": true })))
        }
        _ => Err(acp::Error::method_not_found()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xai_grok_agent::plugins::PluginOrigin as AgentOrigin;
    use xai_grok_agent::plugins::discovery::{PluginId, PluginScope as AgentScope};

    fn make_loaded_plugin(origin: AgentOrigin) -> xai_grok_agent::plugins::LoadedPlugin {
        let root = std::path::PathBuf::from("/tmp/test-plugin");
        xai_grok_agent::plugins::LoadedPlugin {
            name: "test-plugin".to_string(),
            id: PluginId::new(AgentScope::User, &root, "test-plugin"),
            root: root.clone(),
            canonical_root: root,
            scope: AgentScope::User,
            origin,
            trusted: true,
            enabled: true,
            version: Some("1.0.0".to_string()),
            description: None,
            oauth_label: None,
            settings: Vec::new(),
            oauth_accounts: Vec::new(),
            skill_dirs: vec![],
            command_dirs: vec![],
            agent_dirs: vec![],
            hooks_path: None,
            mcp_config_path: None,
            lsp_config_path: None,
            skill_count: 0,
            agent_count: 0,
            skill_names: vec![],
            agent_names: vec![],
            has_hooks: false,
            hook_count: 0,
            has_inline_hooks_only: false,
            mcp_server_count: 0,
            has_inline_mcp_only: false,
            lsp_server_count: 0,
            has_inline_lsp_only: false,
            inline_hooks: None,
            inline_mcp_servers: None,
            inline_lsp_servers: None,
            conflict: None,
            load_error: None,
            sidecar: None,
        }
    }

    /// A plugin whose manifest could not be loaded reaches the pager as a
    /// listed entry carrying its reason, which is the only place a user is
    /// told why the plugin stopped contributing anything.
    #[test]
    fn load_error_reaches_the_plugins_list() {
        let mut plugin = make_loaded_plugin(AgentOrigin::UserGrok);
        plugin.load_error = Some("declares the withdrawn manifest field `plugin`".to_string());
        let info = loaded_plugin_to_info(&plugin);
        assert_eq!(info.load_error, plugin.load_error);
    }

    #[test]
    fn info_carries_origin_and_marketplace_display_name() {
        let plugin = make_loaded_plugin(AgentOrigin::MarketplaceInstall {
            source_name: Some("xAI Official".to_string()),
            git_url: Some("https://example.com/mp.git".to_string()),
        });
        let info = loaded_plugin_to_info(&plugin);
        assert_eq!(
            info.origin,
            Some(PluginOrigin::MarketplaceInstall {
                source_name: Some("xAI Official".to_string()),
                git_url: Some("https://example.com/mp.git".to_string()),
            })
        );
        assert_eq!(info.marketplace_source.as_deref(), Some("xAI Official"));
    }

    #[test]
    fn direct_git_install_gets_git_label() {
        let plugin = make_loaded_plugin(AgentOrigin::MarketplaceInstall {
            source_name: None,
            git_url: Some("https://github.com/obra/superpowers.git".to_string()),
        });
        let info = loaded_plugin_to_info(&plugin);
        assert_eq!(
            info.marketplace_source.as_deref(),
            Some("git: obra/superpowers")
        );
    }

    #[test]
    fn direct_local_install_has_no_marketplace_source() {
        let plugin = make_loaded_plugin(AgentOrigin::MarketplaceInstall {
            source_name: None,
            git_url: None,
        });
        let info = loaded_plugin_to_info(&plugin);
        assert_eq!(info.marketplace_source, None);
        assert_eq!(
            info.origin,
            Some(PluginOrigin::MarketplaceInstall {
                source_name: None,
                git_url: None,
            })
        );
    }

    #[test]
    fn claude_origins_map_to_dto_without_marketplace_source() {
        for (agent_origin, expected) in [
            (
                AgentOrigin::ClaudeMarketplace {
                    marketplace: "mp".to_string(),
                },
                PluginOrigin::ClaudeMarketplace {
                    marketplace: "mp".to_string(),
                },
            ),
            (
                AgentOrigin::ClaudeInstalled {
                    marketplace: Some("mp".to_string()),
                },
                PluginOrigin::ClaudeInstalled {
                    marketplace: Some("mp".to_string()),
                },
            ),
            (AgentOrigin::UserClaude, PluginOrigin::UserClaude),
            (AgentOrigin::ProjectClaude, PluginOrigin::ProjectClaude),
        ] {
            let info = loaded_plugin_to_info(&make_loaded_plugin(agent_origin));
            assert_eq!(info.origin, Some(expected));
            assert_eq!(info.marketplace_source, None);
        }
    }
}
