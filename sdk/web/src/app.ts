// The client itself: connect, list the roster by directory, attach, stream, prompt.
//
// Plain DOM on purpose. This is a proof that the seam works, and a framework
// would put its own idea of state between the wire and the screen — exactly the
// place a second client starts to diverge from the first.
import { GatewayClient, gatewayUrl } from "./client.ts";
import { renderPanel, type PanelAction } from "./panel.ts";
import { ACTIVITY_ROLE, directoryLabel, Roster, sessionLabel } from "./roster.ts";
import { applyTheme, cssVarName, themeByName, type ThemeRole } from "./theme.ts";
import { Transcript } from "./transcript.ts";
import {
  PROTOCOL_VERSION,
  visibleSettingRows,
  type PanelActionResponse,
  type NewSessionResponse,
  type PromptResponse,
  type RequestPermissionRequest,
  type RequestPermissionResponse,
  type RosterChanged,
  type RosterEntry,
  type RosterListResponse,
  type SessionNotification,
  type SettingsListResponse,
} from "./wire.ts";
import { THEMES, type ThemeName } from "@grok-build/theme";

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  className?: string,
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (className) node.className = className;
  return node;
}

function role(name: ThemeRole): string {
  return `var(${cssVarName(name)})`;
}

interface Attached {
  entry: RosterEntry;
  transcript: Transcript;
}

export class App {
  private client: GatewayClient | null = null;
  private readonly roster = new Roster();
  private attached: Attached | null = null;

  /** Dismissers for shared modals awaiting an answer, keyed by tool call id. */
  private readonly pendingPermissions = new Map<string, () => void>();

  private readonly rosterList = el("div", "roster-list");
  private readonly settingsView = el("details", "settings");
  private readonly transcriptView = el("div", "transcript");
  private readonly panelsView = el("div", "panels");
  private readonly permissionsView = el("div", "permissions");
  private readonly sessionHeader = el("header", "session-header");
  private readonly statusLine = el("div", "status");
  private readonly promptInput = el("textarea", "prompt-input");
  private readonly sendButton = el("button", "send");
  private readonly themePicker = el("select", "theme-picker");

  constructor(private readonly root: HTMLElement) {}

  mount(): void {
    const themeName = (localStorage.getItem("grok-theme") ?? "groknight") as ThemeName;
    applyTheme(document.documentElement, themeByName(themeName));

    const layout = el("div", "layout");
    layout.append(this.buildSidebar(themeName), this.buildMain());
    this.root.append(layout);
  }

  // -- chrome ---------------------------------------------------------------

  private buildSidebar(themeName: ThemeName): HTMLElement {
    const side = el("aside", "sidebar");

    const form = el("form", "connect");
    const url = el("input", "connect-url");
    url.type = "text";
    url.value = localStorage.getItem("grok-gateway") ?? "ws://127.0.0.1:2420/ws";
    url.placeholder = "ws://127.0.0.1:2420/ws";
    const secret = el("input", "connect-secret");
    secret.type = "password";
    secret.placeholder = "gateway secret";
    secret.value = localStorage.getItem("grok-secret") ?? "";
    const connect = el("button", "connect-button");
    connect.type = "submit";
    connect.textContent = "Connect";
    form.append(url, secret, connect);
    form.addEventListener("submit", (event) => {
      event.preventDefault();
      localStorage.setItem("grok-gateway", url.value);
      localStorage.setItem("grok-secret", secret.value);
      void this.connect(url.value, secret.value);
    });

    for (const name of Object.keys(THEMES)) {
      const option = el("option");
      option.value = name;
      option.textContent = THEMES[name as ThemeName].display_name;
      if (name === themeName) option.selected = true;
      this.themePicker.append(option);
    }
    this.themePicker.addEventListener("change", () => {
      localStorage.setItem("grok-theme", this.themePicker.value);
      applyTheme(document.documentElement, themeByName(this.themePicker.value));
    });

    side.append(form, this.statusLine, this.themePicker, this.rosterList, this.settingsView);
    return side;
  }

  private buildMain(): HTMLElement {
    const main = el("main", "main");
    const composer = el("form", "composer");
    this.promptInput.placeholder = "Message this session…";
    this.promptInput.rows = 3;
    this.sendButton.type = "submit";
    this.sendButton.textContent = "Send";
    this.sendButton.disabled = true;
    composer.append(this.promptInput, this.sendButton);
    composer.addEventListener("submit", (event) => {
      event.preventDefault();
      void this.sendPrompt();
    });
    this.promptInput.addEventListener("keydown", (event) => {
      if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
        event.preventDefault();
        void this.sendPrompt();
      }
    });
    main.append(
      this.sessionHeader,
      this.permissionsView,
      this.panelsView,
      this.transcriptView,
      composer,
    );
    return main;
  }

  private setStatus(text: string, tone: ThemeRole = "text_secondary"): void {
    this.statusLine.textContent = text;
    this.statusLine.style.color = role(tone);
  }

  // -- wire -----------------------------------------------------------------

  private async connect(base: string, secret: string): Promise<void> {
    this.client?.close();
    this.setStatus("connecting…");
    const client = new GatewayClient(() => new WebSocket(gatewayUrl(base, secret)));
    this.client = client;

    client.onNotification((method, params) => this.onNotification(method, params));
    // Answer, never ignore. `request_permission` has no timeout on the agent
    // side (`permission/prompter.rs`), so silence parks the session actor and
    // the turn never ends. Anything else this client does not implement is
    // refused, which at least releases the caller.
    client.onRequest((method, params) => {
      if (method === "session/request_permission") {
        return this.askPermission(params as RequestPermissionRequest);
      }
      return undefined;
    });

    try {
      await client.connect();
      await client.request("initialize", {
        protocolVersion: PROTOCOL_VERSION,
        clientCapabilities: {
          fs: { readTextFile: false, writeTextFile: false },
          terminal: false,
        },
        clientInfo: { name: "grok-web", version: "0.1.0" },
      });
      await this.refreshRoster();
      await this.refreshSettings();
      this.setStatus("connected", "accent_success");
    } catch (e) {
      this.setStatus(`connection failed: ${String(e)}`, "accent_error");
    }
  }

  private async refreshRoster(): Promise<void> {
    if (!this.client) return;
    const response = (await this.client.ext("x.ai/sessions/list", {})) as RosterListResponse;
    this.roster.replace(response.sessions ?? []);
    this.renderRoster();
  }

  /**
   * Read the settings catalog and draw the rows the wire says a non-terminal
   * client should draw. Read-only: writing is `x.ai/settings/set` and is the
   * obvious next step, but it is not what this client is proving.
   */
  private async refreshSettings(): Promise<void> {
    if (!this.client) return;
    const response = (await this.client.ext("x.ai/settings/list", {})) as SettingsListResponse;
    const rows = visibleSettingRows(response.catalog?.rows ?? []);
    const skipped = (response.catalog?.rows ?? []).length - rows.length;
    this.settingsView.replaceChildren();
    const summary = el("summary");
    summary.textContent = `Settings — ${rows.length} shown, ${skipped} terminal-only`;
    this.settingsView.append(summary);
    for (const row of rows) {
      const line = el("div", "setting-row");
      const key = el("span", "setting-label");
      key.textContent = row.label;
      key.title = row.key;
      const value = el("span", "setting-value");
      const lock = response.state?.locks?.[row.key];
      value.textContent = lock
        ? `locked — ${lock.reason}`
        : String(response.state?.values?.[row.key] ?? "—");
      if (lock) value.style.color = role("warning");
      line.append(key, value);
      this.settingsView.append(line);
    }
  }

  private onNotification(method: string, params: unknown): void {
    if (method === "x.ai/sessions/changed") {
      this.roster.apply((params ?? {}) as RosterChanged);
      this.renderRoster();
      return;
    }
    // One dispatch for both carriers: the standard ACP notification and the
    // grok extension one share the `sessionUpdate` tag and the same envelope,
    // so the carrier is not a fork in the client.
    if (
      method === "session/update" ||
      method === "x.ai/session/update" ||
      method === "x.ai/session_notification"
    ) {
      const notification = params as SessionNotification | undefined;
      if (!notification?.update) return;
      if (!this.attached || notification.sessionId !== this.attached.entry.sessionId) return;
      const update = notification.update;
      if (update.sessionUpdate === "interaction_resolved") {
        // Someone else answered the shared modal. Take ours down.
        const id = String((update as Record<string, unknown>)["tool_call_id"] ?? "");
        this.pendingPermissions.get(id)?.();
        return;
      }
      this.attached.transcript.apply(update);
      this.renderSession();
    }
  }

  /**
   * Draw a shared permission modal and answer it.
   *
   * The options are rendered from the `options` array as sent, never from a
   * hardcoded id list: which options exist depends on the tool and on the
   * client type the leader registered, and a client that assumes them will
   * eventually answer with an id the agent does not know.
   *
   * The modal is shared. Another attached client can answer first, in which
   * case `interaction_resolved` arrives and dismisses this one; the promise is
   * then settled with `cancelled`, which the agent ignores because it already
   * has its answer.
   */
  private askPermission(request: RequestPermissionRequest): Promise<RequestPermissionResponse> {
    return new Promise((resolve) => {
      const toolCallId = request.toolCall?.toolCallId ?? "";
      const card = el("div", "permission");
      const title = el("div", "permission-title");
      title.textContent = request.toolCall?.title ?? "Permission requested";
      const options = el("div", "permission-options");
      const settle = (outcome: RequestPermissionResponse): void => {
        this.pendingPermissions.delete(toolCallId);
        card.remove();
        resolve(outcome);
      };
      for (const option of request.options ?? []) {
        const button = el("button", `permission-option permission-${option.kind}`);
        button.textContent = option.name;
        button.addEventListener("click", () =>
          settle({ outcome: { outcome: "selected", optionId: option.optionId } }),
        );
        options.append(button);
      }
      card.append(title, options);
      this.pendingPermissions.set(toolCallId, () => settle({ outcome: { outcome: "cancelled" } }));
      this.permissionsView.append(card);
    });
  }

  /**
   * Create a session rooted at `cwd` and attach to it.
   *
   * `cwd` is a parameter of `session/new`, never process state, which is why a
   * browser can put a session anywhere the leader can reach — and why "switch
   * instance" is "attach to another session", not "repoint this one".
   */
  private async createSession(cwd: string): Promise<void> {
    if (!this.client) return;
    this.setStatus(`creating a session in ${cwd}…`);
    try {
      const created = (await this.client.request("session/new", {
        cwd,
        mcpServers: [],
      })) as NewSessionResponse;
      await this.refreshRoster();
      const entry = this.roster.get(created.sessionId);
      if (entry) await this.attach(entry);
    } catch (e) {
      this.setStatus(`could not create a session: ${String(e)}`, "accent_error");
    }
  }

  private async attach(entry: RosterEntry): Promise<void> {
    if (!this.client) return;
    this.attached = { entry, transcript: new Transcript() };
    this.renderSession();
    this.setStatus(`loading ${entry.sessionId}…`);
    try {
      // `cwd` comes straight off the roster row. That it is there at all is the
      // reason a second client can attach to a session it did not create.
      await this.client.request("session/load", {
        sessionId: entry.sessionId,
        cwd: entry.cwd,
        mcpServers: [],
      });
      this.sendButton.disabled = false;
      this.setStatus(`attached to ${entry.sessionId}`, "accent_success");
    } catch (e) {
      this.setStatus(`load failed: ${String(e)}`, "accent_error");
    }
    this.renderSession();
  }

  private async sendPrompt(): Promise<void> {
    const text = this.promptInput.value.trim();
    if (!text || !this.client || !this.attached) return;
    this.promptInput.value = "";
    this.sendButton.disabled = true;
    this.setStatus("running…", "accent_running");
    try {
      const response = (await this.client.request("session/prompt", {
        sessionId: this.attached.entry.sessionId,
        prompt: [{ type: "text", text }],
        // The agent echoes `promptId` on every notification it emits for this
        // turn, which is how a client tells a cancelled turn's chunks from the
        // next turn's. Not used for filtering yet, but omitting it would throw
        // the information away at the source.
        _meta: { promptId: crypto.randomUUID() },
      })) as PromptResponse;
      this.setStatus(`turn ended: ${response.stopReason}`, "accent_success");
    } catch (e) {
      this.setStatus(`prompt failed: ${String(e)}`, "accent_error");
    } finally {
      this.sendButton.disabled = false;
    }
  }

  private async sendPanelAction(plugin: string, action: PanelAction): Promise<void> {
    if (!this.client || !this.attached) return;
    try {
      const response = (await this.client.ext("x.ai/plugins/panel_action", {
        sessionId: this.attached.entry.sessionId,
        plugin,
        panelId: action.panelId,
        buttonId: action.buttonId,
        inputs: action.inputs,
      })) as PanelActionResponse;
      // `delivered` means handed to the sidecar, not acted on — delivery is
      // fire-and-forget past that point. `false` means the session or the
      // plugin is gone, so the panel on screen is stale.
      if (!response.delivered) {
        this.setStatus(`panel action not delivered: ${plugin}/${action.panelId}`, "warning");
      }
    } catch (e) {
      this.setStatus(`panel action failed: ${String(e)}`, "accent_error");
    }
  }

  // -- rendering ------------------------------------------------------------

  private renderRoster(): void {
    this.rosterList.replaceChildren();
    const groups = this.roster.groups();
    if (groups.length === 0) {
      const empty = el("p", "empty");
      empty.textContent = "No sessions on this leader yet.";
      this.rosterList.append(empty);
      return;
    }
    for (const group of groups) {
      const section = el("section", "roster-group");
      const heading = el("h2", "roster-cwd");
      heading.textContent = directoryLabel(group.cwd);
      heading.title = group.cwd;
      const path = el("div", "roster-cwd-full");
      path.textContent = group.cwd;
      // A new session in a directory that already has one. Arbitrary directories
      // need a picker, and a picker needs `x.ai/fs/list`, which resolves against
      // the leader's launch directory rather than the session's — so this client
      // can only offer roots the roster already names.
      const add = el("button", "roster-new");
      add.type = "button";
      add.textContent = "+ session here";
      add.title = `New session in ${group.cwd}`;
      add.addEventListener("click", () => void this.createSession(group.cwd));
      section.append(heading, path, add);

      for (const entry of group.sessions) {
        const row = el("button", "roster-row");
        row.type = "button";
        if (this.attached?.entry.sessionId === entry.sessionId) row.classList.add("current");
        const dot = el("span", "roster-dot");
        dot.style.background = role(ACTIVITY_ROLE[entry.activity]);
        dot.title = entry.activity;
        const label = el("span", "roster-label");
        label.textContent = sessionLabel(entry);
        const meta = el("span", "roster-meta");
        meta.textContent = [
          entry.isWorktree ? "worktree" : "",
          entry.resident ? "" : "dormant",
          entry.modelId ?? "",
        ]
          .filter(Boolean)
          .join(" · ");
        row.append(dot, label, meta);
        row.addEventListener("click", () => void this.attach(entry));
        section.append(row);
      }
      this.rosterList.append(section);
    }
  }

  private renderSession(): void {
    this.sessionHeader.replaceChildren();
    this.transcriptView.replaceChildren();
    this.panelsView.replaceChildren();
    if (!this.attached) {
      const hint = el("p", "empty");
      hint.textContent = "Pick a session on the left.";
      this.transcriptView.append(hint);
      return;
    }

    const { entry, transcript } = this.attached;
    const title = el("h1", "session-title");
    title.textContent = sessionLabel(entry);
    const cwd = el("div", "session-cwd");
    cwd.textContent = entry.cwd;
    this.sessionHeader.append(title, cwd);

    for (const panel of transcript.panels.values()) {
      const handle = renderPanel(panel.plugin, panel.viewModel, (action) =>
        void this.sendPanelAction(panel.plugin, action),
      );
      this.panelsView.append(handle.element);
    }

    for (const item of transcript.entries) {
      if (item.kind === "message") {
        const block = el("article", `message message-${item.role}`);
        const who = el("div", "message-role");
        who.textContent = item.role;
        const body = el("div", "message-text");
        body.textContent = item.text;
        block.append(who, body);
        this.transcriptView.append(block);
      } else {
        const block = el("article", `tool tool-${item.status}`);
        const head = el("div", "tool-title");
        head.textContent = `${item.title} — ${item.status}`;
        block.append(head);
        if (item.output) {
          const out = el("pre", "tool-output");
          out.textContent = item.output;
          block.append(out);
        }
        this.transcriptView.append(block);
      }
    }
    this.transcriptView.scrollTop = this.transcriptView.scrollHeight;
  }
}
