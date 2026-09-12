# Durable watermark for exactly-once response-boundary delivery

Status: **implemented** (follow-up to `c07460e8`). This doc is the plan *and* the record of
what was built. The core guarantee plumbing is in and compiles clean; the crash-window
reasoning below is why each piece exists.

Implemented in:
- `src/session/helpers/session_prompt_delivery.rs` — durable types + load/save + reconcile.
- `src/session/acp_session_impl/prompt_queue.rs` — write-ahead in `drain_response_boundary_work`
  + `reinject_restored_delivered_work`.
- `src/session/acp_session_impl/spawn.rs` — resume restore + reconcile against the conversation.
- `src/session/acp_session.rs` — `State.prompt_delivery_watermark` + `State.prompt_delivered_work`.

## 0. The guarantee we want

Each `/loop` fire and each queued parent message, once **bound to a response boundary and
committed** (drained from `pending_inputs` into `pending_interjections`), must be delivered
**exactly once** across the lifetime of its prompt — even if the process crashes and
resumes one or more times mid-prompt.

`c07460e8` already:
- advances `AgentTask.response_seq` once per committed response boundary;
- drains response-bound work (`drain_response_boundary_work`) after each tool batch,
  before the next model call, promoting `SchedulerFired` / `ParentAgentMessage { .. }`
  (excluding the running front) into interjections for the next request.

What it does **not** do is make that commit durable across a crash.

## 1. Why the current code is not exactly-once

The commit path (per prompt `P`, up to response `N`):

```
response boundary N committed:
  response_seq: N-1 -> N            (in-memory AgentTask field)
  work N+1: pending_inputs -> pending_interjections   (in-memory)
  next model call injects work N+1 into conversation   (persisted to conversation.json)
```

The crash windows:

| Crash happens… | `response_seq` | work N+1 in conversation | work N+1 in `pending_inputs`/`pending_interjections` | Result on resume |
|---|---|---|---|---|
| before commit | `N-1` | no | yes (in queue) | queue rebuilt from conversation → work still queued → **re-drained, delivered** ✓ |
| after commit, before injection | `N` (advanced) | no | **gone** (moved to interjections, in-memory) | work **LOST** ✗ |
| after injection | `N` | yes | no | delivered ✓ |

Two observations:

1. **`response_seq` lives only on the in-memory `AgentTask`.** On resume it resets to `0`,
   so the response-boundary counter is not continuous across a crash.
2. **`pending_inputs` is rebuilt from the conversation on resume**, and the conversation
   only contains work that was *injected*. Work that was *drained but not yet injected* is
   gone — the queue is not persisted.

Because of (2), the "after commit, before injection" window silently drops work.

## 2. What is missing (both required for exactly-once)

### A. Persist committed-but-not-yet-delivered work (write-ahead)

Durably record the work that left `pending_inputs` at a committed boundary **before**
`response_seq` is advanced, so a crash can't lose it.

Two implementation options:

- **A1 — persist the whole queue.** Simplest conceptually, hardest in practice:
  `InputItem` carries live `oneshot::Sender<>` channels (`respond_to`, `persist_ack`,
  `parsed_prompt_tx`, `initial_child_prompt_ready`) that **cannot be serialized**. Would
  need to drop them on persist and re-resolve senders on resume — fragile, and a resumed
  client that never re-acks would leak a pending turn.
- **A2 — persist only the drained-not-injected subset.** A dedicated file
  (`prompt_delivered/{session}.json`) listing entries
  `(prompt_id, response_seq, work_prompt_id)`. Small, targeted, no channel problem (we
  only store ids + a serializable prompt snapshot). **Recommended.**

### B. Write-ahead watermark `(prompt_id, response_seq)`

Persist the last committed `(prompt_id, response_seq)` **before** advancing the in-memory
counter, and restore it on resume so the counter is continuous. This is the durable
record of "responses 1..N for prompt P are committed".

### Why both — the crash-window table

| | no A | no B | A + B |
|---|---|---|---|
| after commit, before injection | ✓ (A re-injects) | ✗ **LOST** (B says committed, nothing to re-inject) | ✓ once |
| after injection (work in conversation) | ✗ **DOUBLE** (B absent → re-injects work already in conversation) | ✓ (in conversation) | ✓ once |

`A` alone → double-delivery; `B` alone → lost work; **`A` + `B` → exactly once.**

## 3. Coupling: this only matters if in-flight prompts are resumed

Today a resumed session rebuilds `State` (`running_task: None`), rebuilds the queue from
the conversation, and **waits for a new prompt** — it does not continue an in-flight turn.
So with the current resume behavior:

- a completed prompt's work is already in the conversation (delivered);
- an abandoned prompt's work was never injected (lost, but the prompt is dead anyway).

The watermark becomes meaningful only when the session can **resume an in-flight prompt `P`
and keep continuing it across a crash**. `A` + `B` + in-flight continuation are one
feature; ship them together. Implementing `B` alone today is a persisted no-op (defensive,
future-proofing) — do not ship it in isolation.

## 4. Implementation steps (A2 + B + resume reconcile) — done

1. **Durable record.** `PromptDeliveryState { watermark: Option<PromptDeliveryWatermark>,
   pending: Vec<PromptDeliveryEntry> }` serialized to a side-file
   `prompt_delivery_state.json` in the session dir (mirrors the recap / title-refresh
   watermark pattern: `load_*_watermark` / `save_*_watermark` best-effort helpers). One file
   holds both the watermark and the pending work — no separate A2 file, no summary.json.
2. **Write-ahead in the drain.** `drain_response_boundary_work` now:
   (a) re-injects restored work (`reinject_restored_delivered_work`);
   (b) captures `new_seq` from the *current* in-memory counter **before** the bump;
   (c) partitions `pending_inputs -> to_promote`;
   (d) builds a `PromptDeliveryEntry` per promoted item (`prompt_delivery_entry`) and calls
       `delivery.commit(running_id, new_seq, entries)` + `save_prompt_delivery_state`
       **before** `advance_response_boundary()`;
   (e) advances the in-memory counter; (f) enqueues the promoted items as interjections.
   `commit` bumps the watermark to at least `new_seq` for `prompt_id` and appends the entries.
3. **Resume restore + reconcile.** `spawn.rs` loads the state, reconciles against the loaded
   conversation (`reconcile(|e| conversation_contains_text(&conversation, &e.text))`), drops
   the delivered entries from the durable record (so it does not grow), and seeds
   `State.prompt_delivery_watermark` (continuous counter) + `State.prompt_delivered_work`
   (the not-yet-delivered entries, re-injected at the next drain).
4. **Tests.** Unit tests in `session_prompt_delivery.rs`: commit monotonicity, retain,
   pure `reconcile` split, JSON round-trip, missing/corrupt-file defaults, and
   `conversation_contains_text` (user-only match).

## 5. Risks / open questions

- **`InputItem` channels** (A1) make full-queue persistence fragile; we persist only the
  drained-not-injected subset (`PromptDeliveryEntry`), so no live-channel problem.
- **Persistence is best-effort, not fsync-guaranteed.** `save_prompt_delivery_state` logs on
  error and returns; it is not routed through the persistence actor's sequential channel. So a
  crash *immediately after* `commit` returns but *before* the OS flushes the side-file can
  still lose the record — the loss window is narrowed (write-ahead ordering is preserved) but
  not eliminated. Closing it fully means making the write-ahead durable (fsync) before the
  seq bump, coordinated with the persistence-actor flush model.
- **Images dropped.** `PromptDeliveryEntry.text` carries only the rendered text; image
  attachments are not stored. Re-injection on resume injects text only. Fine for `/loop` /
  parent-message text; images would need a serializable snapshot to round-trip.
- **`conversation_contains_text` is a substring match** on user-content text (interjections
  land as user messages). Tolerates render whitespace; could in theory over-match, but the
  injected text is distinctive.
- **Scope discipline**: paging/rewind/feedback semantics untouched — the watermark only reads
  the conversation + a small side-file.
