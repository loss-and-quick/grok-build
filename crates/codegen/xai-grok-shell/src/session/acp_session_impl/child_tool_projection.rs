use xai_grok_sampling_types::ToolSpec;
use xai_grok_tools::types::tool::ToolKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ChildToolProjection {
    Rebuilt,
    VerbatimMirror,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ChildMessagingGrant {
    Granted,
    Ungranted,
}

/// DELIBERATE FORK DIVERGENCE — do not "restore" this to upstream on a merge.
///
/// Upstream keeps the active-message tool on a child only under an explicit
/// [`ChildMessagingGrant::Granted`]. This fork runs with
/// `subagents.max_depth = 3`, where a depth-1 agent really does own children of
/// its own — and stripping the tool from it leaves it able to start work it
/// cannot then correct, with no fallback but kill-and-respawn.
///
/// So a child that *can* spawn keeps the tool as well, grant or no grant: `task`
/// in its own specs is the honest test, being the tool that mints the ids the
/// active-message tool takes. A child that cannot spawn still loses it unless
/// granted, because it can never hold an id to point it at. Ownership, not
/// depth, is what actually protects anyone here, and the coordinator enforces
/// that on every send — a sender may only name a child its own session started.
///
/// If a future sync conflicts on this file, keep upstream's filter and re-apply
/// the `can_spawn` term.
pub(super) fn child_safe_tool_specs(
    specs: Vec<ToolSpec>,
    projection: ChildToolProjection,
    messaging_grant: ChildMessagingGrant,
    kind_for_name: impl Fn(&str) -> Option<ToolKind>,
) -> Vec<ToolSpec> {
    // Unknown names are parent-only capabilities; messaging survives a grant or a spawner.
    let can_spawn = specs
        .iter()
        .any(|spec| kind_for_name(&spec.name) == Some(ToolKind::Task));
    let keeps_messaging = messaging_grant == ChildMessagingGrant::Granted || can_spawn;
    match projection {
        ChildToolProjection::Rebuilt | ChildToolProjection::VerbatimMirror => specs
            .into_iter()
            .filter(|spec| match kind_for_name(&spec.name) {
                Some(ToolKind::ActiveAgentMessage) => keeps_messaging,
                Some(_) => true,
                None => false,
            })
            .collect(),
    }
}

#[cfg(test)]
#[path = "child_tool_projection_tests.rs"]
mod tests;
