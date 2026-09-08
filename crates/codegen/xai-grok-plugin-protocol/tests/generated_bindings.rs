//! Drift guard for `sdk/plugin/src/generated/`.
//!
//! ts-rs's own `#[ts(export)]` writes the checked-in TypeScript from a hidden
//! per-type test, so `cargo test` used to rewrite 94 tracked files and still
//! report success: a Rust-side change to a plugin DTO left no failure behind,
//! only an unreviewed diff nobody was looking for. The `export` attribute is
//! therefore gone from the DTOs, and this test is the only writer. It renders
//! the bindings into `CARGO_TARGET_TMPDIR` and compares, the same trick the
//! theme palette uses (`xai-grok-pager-render/src/theme/tokens.rs`), so a
//! change in Rust and a hand-edit of the TypeScript both fail here and are both
//! fixed by the command in the message.
//!
//! `export_to` is gone too: it pointed four `..` above ts-rs's default base, and
//! a path that climbs out of the export directory cannot be redirected into a
//! scratch directory safely. The destination now lives here, once.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ts_rs::{Config, TS};
use xai_grok_plugin_protocol::*;

/// The checked-in artifact, relative to the repo root.
const ARTIFACT_REL_PATH: &str = "sdk/plugin/src/generated";

const REGENERATE_CMD: &str =
    "GROK_WRITE_PLUGIN_BINDINGS=1 cargo test -p xai-grok-plugin-protocol --test generated_bindings";

/// Every root the bindings are generated from.
///
/// `export_all` follows each root's dependencies, so a new DTO reachable from
/// one of these is covered without being named. A root that is dropped from this
/// list leaves its `.ts` file behind with nothing to compare it to, which the
/// test below reports as an extra file rather than passing quietly.
fn render_into(dir: &Path) {
    let cfg = Config::new().with_out_dir(dir);
    macro_rules! export_all {
        ($($t:ty),+ $(,)?) => {$(
            <$t as TS>::export_all(&cfg)
                .unwrap_or_else(|e| panic!("exporting {}: {e}", stringify!($t)));
        )+};
    }
    export_all!(
        GateKindDto,
        DecisionDto,
        LogLevelDto,
        EventName,
        HookEnvelopeCommon,
        SubagentStopPhaseDto,
        BackgroundTaskTypeDto,
        StopFailureKindDto,
        StopCancelledReasonDto,
        CancelledByDto,
        StopBackgroundTaskDto,
        StopSessionCronDto,
        ProviderResponseToolCallDto,
        SessionStartPayload,
        SessionEndPayload,
        StopPayload,
        StopFailurePayload,
        StopCancelledPayload,
        PreToolUsePayload,
        PostToolUsePayload,
        PostToolUseFailurePayload,
        PermissionDeniedPayload,
        UserPromptSubmitPayload,
        NotificationPayload,
        SubagentStartPayload,
        SubagentStopPayload,
        PreCompactPayload,
        PostCompactPayload,
        ProviderRequestPayload,
        ProviderResponsePayload,
        ProviderErrorPayload,
        SubagentResolvePayload,
        ResolveCredentialPayload,
        RefreshCredentialPayload,
        StartOauthFlowPayload,
        HostCapabilities,
        InitializeParams,
        InitializeResult,
        HookInvokeParams,
        HookInvokeResult,
        PluginCredentialDto,
        ToolDescriptorDto,
        ToolCallContextDto,
        ToolInvokeParams,
        ToolInvokeResult,
        ToolCancelParams,
        CommandDescriptorDto,
        CommandInvokeParams,
        CommandInvokeResult,
        ShutdownParams,
        LogEmitParams,
        StorageGetParams,
        StorageGetResult,
        StorageSetParams,
        StorageSetResult,
        StorageDeleteParams,
        StorageDeleteResult,
        StorageListParams,
        StorageListResult,
        ConfigGetParams,
        ConfigGetResult,
        AgentSpawnParams,
        AgentSpawnResult,
        AgentStatusDto,
        AgentDescriptorDto,
        AgentWaitParams,
        AgentWaitResult,
        AgentEventsParams,
        AgentEventKindDto,
        AgentEventDto,
        AgentEventsResult,
        AgentListParams,
        AgentListResult,
        AgentCancelParams,
        AgentCancelOutcomeDto,
        AgentCancelResult,
        AgentSendParams,
        AgentSendResult,
        AgentMessageParams,
        AgentMessageOutcomeDto,
        AgentMessageResult,
        PanelTone,
        PanelStatusItem,
        PanelButton,
        PanelBlock,
        PanelViewModel,
        PanelPublishResult,
        PanelCloseParams,
        PanelCloseResult,
        PanelActionParams,
        AuthPublishUrlParams,
        AuthPublishUrlResult,
        AuthAwaitCodeParams,
        AuthAwaitCodeResult,
    );
}

/// Absolute path of the checked-in artifact directory.
fn artifact_dir() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/codegen/xai-grok-plugin-protocol.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join(ARTIFACT_REL_PATH)
}

/// Every `.ts` file in `dir`, keyed by file name. Non-`.ts` entries (the
/// `.gitkeep` that keeps the directory in git) are not ours to compare or erase.
fn bindings_in(dir: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "ts") {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let body = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
        out.insert(name, body);
    }
    out
}

/// The first line on which two files disagree, so a one-line drift is not
/// buried under a whole-file diff.
fn first_difference(checked_in: &str, from_rust: &str) -> String {
    let mismatch = checked_in
        .lines()
        .zip(from_rust.lines())
        .enumerate()
        .find(|(_, (a, b))| a != b);
    match mismatch {
        Some((index, (found, want))) => format!(
            "line {}:\n    checked in: {found}\n    from Rust:  {want}",
            index + 1
        ),
        None => format!(
            "line count differs: checked in {}, from Rust {}",
            checked_in.lines().count(),
            from_rust.lines().count()
        ),
    }
}

#[test]
fn generated_plugin_bindings_match_the_rust_dtos() {
    let artifact_dir = artifact_dir();

    if std::env::var_os("GROK_WRITE_PLUGIN_BINDINGS").is_some() {
        std::fs::create_dir_all(&artifact_dir).expect("create the generated directory");
        let before = bindings_in(&artifact_dir);
        render_into(&artifact_dir);
        // A type that stopped being exported leaves a file no generator owns any
        // more; regenerating has to remove it or the next run compares against a
        // ghost.
        let after = bindings_in(&artifact_dir);
        for stale in before.keys().filter(|name| !after.contains_key(*name)) {
            std::fs::remove_file(artifact_dir.join(stale)).expect("remove a stale binding");
        }
        return;
    }

    // Rendering never touches the source tree: the scratch directory is inside
    // the build's own target directory.
    let scratch = Path::new(env!("CARGO_TARGET_TMPDIR")).join("plugin-bindings-guard");
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).expect("create the scratch directory");
    render_into(&scratch);

    let from_rust = bindings_in(&scratch);
    let checked_in = bindings_in(&artifact_dir);

    let missing: Vec<&str> = from_rust
        .keys()
        .filter(|name| !checked_in.contains_key(*name))
        .map(String::as_str)
        .collect();
    let extra: Vec<&str> = checked_in
        .keys()
        .filter(|name| !from_rust.contains_key(*name))
        .map(String::as_str)
        .collect();
    let changed: Vec<String> = from_rust
        .iter()
        .filter_map(|(name, want)| {
            let found = checked_in.get(name)?;
            (found != want).then(|| format!("  {name} {}", first_difference(found, want)))
        })
        .collect();

    if missing.is_empty() && extra.is_empty() && changed.is_empty() {
        return;
    }

    let mut detail = String::new();
    if !missing.is_empty() {
        detail.push_str(&format!("  never checked in: {missing:?}\n"));
    }
    if !extra.is_empty() {
        detail.push_str(&format!(
            "  checked in but no longer generated: {extra:?}\n"
        ));
    }
    for line in &changed {
        detail.push_str(line);
        detail.push('\n');
    }
    panic!(
        "{ARTIFACT_REL_PATH} disagrees with the Rust DTOs:\n{detail}\n\
         The Rust types are the definition. If the change is intended, \
         regenerate with: {REGENERATE_CMD}"
    );
}
