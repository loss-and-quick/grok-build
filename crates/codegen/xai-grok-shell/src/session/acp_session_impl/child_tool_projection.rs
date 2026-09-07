use xai_grok_sampling_types::ToolSpec;
use xai_grok_tools::implementations::grok_build::SEND_SUBAGENT_MESSAGE_TOOL_NAME;
use xai_grok_tools::types::tool::ToolKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ChildToolProjection {
    Rebuilt,
    VerbatimMirror,
}

/// DELIBERATE FORK DIVERGENCE — do not "restore" this to upstream on a merge.
///
/// Upstream drops the active-message tool from every child unconditionally: in
/// their configuration only the root session spawns, so no child ever owns one
/// to steer, and the strip costs nothing. This fork runs with
/// `subagents.max_depth = 3`, where a depth-1 agent really does own children of
/// its own — and stripping the tool from it leaves it able to start work it
/// cannot then correct, with no fallback but kill-and-respawn.
///
/// So the strip is kept and narrowed: a child that cannot spawn still loses the
/// tool, because it can never hold an id to point it at. A child that *can*
/// spawn keeps it. Ownership, not depth, is what actually protects anyone here,
/// and the coordinator enforces that below on every send — a sender may only
/// name a child its own session started.
///
/// If a future sync conflicts on this file, the upstream side is the
/// unconditional filter; keep this one and re-apply the `can_spawn` guard.
pub(super) fn child_safe_tool_specs(
    specs: Vec<ToolSpec>,
    projection: ChildToolProjection,
    kind_for_name: impl Fn(&str) -> Option<ToolKind>,
) -> Vec<ToolSpec> {
    // The filter matches by kind so a renamed tool is still caught, and by canonical name when the child bridge no longer registers the tool
    // VerbatimMirror leaves every other field of the parent's ToolSpecs unchanged so the child's request still hits the parent's radix cache
    // ask_user_question is stripped at the subagent mirror call sites, so forks that are not subagents keep it
    // `task` in the child's own specs is the honest test of whether it may spawn: it is the tool that mints the ids the active-message tool takes
    let can_spawn = specs
        .iter()
        .any(|spec| kind_for_name(&spec.name) == Some(ToolKind::Task));
    if can_spawn {
        return specs;
    }
    match projection {
        ChildToolProjection::Rebuilt | ChildToolProjection::VerbatimMirror => specs
            .into_iter()
            .filter(|spec| {
                kind_for_name(&spec.name) != Some(ToolKind::ActiveAgentMessage)
                    && spec.name != SEND_SUBAGENT_MESSAGE_TOOL_NAME
            })
            .collect(),
    }
}

#[cfg(test)]
#[path = "child_tool_projection_tests.rs"]
mod tests;
