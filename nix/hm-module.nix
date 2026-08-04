# Home Manager integration: `programs.grok-build.enable = true;` installs the
# `grok` binary and can deploy its config + plugins declaratively.
#
# Namespaced under `programs.*` to match the upstream Home Manager convention
# (e.g. `programs.opencode`) rather than the user's own dotfiles' private
# `module.<name>` wrapping style (home/modules/opencode/default.nix there) —
# that style wraps an *existing* upstream module; this one, like opencode's
# own upstream module, defines the option namespace from scratch.
#
# `self` is threaded in from the flake so `package` can default to this
# repo's own build without the module having to know a system — mirrors
# Magic_V2Ray's nix/nixos-module.nix (`programs.kasumi-proxy`).
{self}: {
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.programs.grok-build;
  inherit (lib) mkEnableOption mkOption mkIf literalExpression types;

  tomlFormat = pkgs.formats.toml {};

  pluginFile = name: path:
    lib.nameValuePair ".grok/plugins/${name}" {source = path;};

  agentFile = name: path:
    lib.nameValuePair ".grok/agents/${name}.md" {source = path;};

  # The basename must match the script's own `meta.name` — the registry
  # cross-validates them and rejects the workflow otherwise.
  workflowFile = name: path:
    lib.nameValuePair ".grok/workflows/${name}.rhai" {source = path;};

  # The canonical reasoning-effort values `ReasoningEffort` deserializes
  # (crates/codegen/xai-grok-sampling-types/src/types.rs). Spelled as an enum so
  # a typo fails at nix eval rather than at grok's config parse.
  reasoningEffortType = types.enum [
    "none"
    "minimal"
    "low"
    "medium"
    "high"
    "xhigh"
    "max"
  ];

  # The auth schemes `ProviderAuthScheme` deserializes
  # (crates/codegen/xai-grok-config-types/src/provider.rs) — snake_case, matching
  # the sampler's own `AuthScheme`, so `x_api_key` and not the `x-api-key` header
  # spelling. An enum so that distinction fails at nix eval rather than at grok's
  # config parse.
  providerAuthSchemeType = types.enum [
    "bearer"
    "x_api_key"
    "google_api_key"
  ];

  # The `thinking` dialects `ThinkingDialect` deserializes
  # (crates/codegen/xai-grok-sampling-types/src/types.rs): the two mode names, or
  # a `{ budget_tokens = N; }` attrset for the explicit-budget dialect. Folding
  # the number into the value rather than pairing a `"budget"` name with a
  # separate option means a budget dialect with no budget is unspellable.
  thinkingDialectType = types.either
    (types.enum ["adaptive" "off"])
    (types.submodule {
      options.budget_tokens = mkOption {
        type = types.ints.positive;
        example = 8000;
        description = ''
          Thinking budget in tokens. The Messages API requires at least 1024,
          and strictly less than the request's `max_tokens`.
        '';
      };
    });

  # One `[[provider]]` registry entry. Fields mirror
  # `xai_grok_config_types::provider::ProviderConfig`
  # (crates/codegen/xai-grok-config-types/src/provider.rs) 1:1, including its
  # serde snake_case field names — the attrs are emitted straight to TOML.
  providerType = types.submodule {
    options = {
      id = mkOption {
        type = types.str;
        description = "Stable identifier, used as the `<id>/` routing prefix.";
      };
      format = mkOption {
        type = types.enum ["chat_completions" "responses" "messages" "gemini"];
        default = "chat_completions";
        description = ''
          Wire format this provider speaks: `chat_completions` (OpenAI Chat
          Completions), `responses` (OpenAI Responses), `messages` (Anthropic
          Messages), or `gemini` (Google Gemini).
        '';
      };
      base_url = mkOption {
        type = types.str;
        example = "https://example.test/v1";
        description = ''
          Endpoint base URL. May itself be a `$VAR` or `{file:/path}` secret
          reference (grok expands those in provider credential fields).
        '';
      };
      api_key = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          Credential sent per `auth_scheme`, defaulting to the format's own
          scheme (Bearer / `x-api-key` / `x-goog-api-key`). Prefer a
          `{file:/path}` reference over a literal so the secret never lands in
          the world-readable Nix store. `null` (the default) omits the field
          entirely.
        '';
      };
      auth_scheme = mkOption {
        type = types.nullOr providerAuthSchemeType;
        default = null;
        example = "bearer";
        description = ''
          Which header `api_key` rides in, overriding the wire format's default.

          The auth scheme belongs to the *credential* as much as to the format.
          An Anthropic-format endpoint authenticated by an API key wants
          `x-api-key`, which is why that is the `messages` default — but the same
          endpoint authenticated by an OAuth bearer (a subscription token, e.g.
          one a credential plugin resolves) is accepted only in
          `Authorization: Bearer`; offered as `x-api-key` it is not a valid key
          and every request is rejected, no matter how often the token is
          refreshed. Set `"bearer"` for that case.

          `null` (the default) keeps the format's own scheme, so a provider that
          does not care never mentions this.
        '';
      };
      headers = mkOption {
        type = types.attrsOf types.str;
        default = {};
        description = ''
          Extra request headers applied verbatim. Values may be secret
          references. Empty (the default) omits the table.
        '';
      };
      proxy = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Per-provider HTTP(S) proxy URL. `null` omits the field.";
      };
      models = mkOption {
        type = types.listOf types.str;
        default = [];
        description = ''
          Bare model slugs this provider serves. Each is exposed as both
          `<id>/<model>` and the bare `<model>`. Empty (the default) omits it.
        '';
      };
      context_window = mkOption {
        type = types.nullOr types.ints.positive;
        default = null;
        description = ''
          Default context window for this provider's models when a model does
          not otherwise supply one. `null` omits the field.
        '';
      };
      max_completion_tokens = mkOption {
        type = types.nullOr types.ints.positive;
        default = null;
        example = 64000;
        description = ''
          Maximum output tokens this provider's models may be asked to generate.
          Like `context_window`, it describes the *endpoint*, so every model the
          provider serves inherits it; override a single model with a
          `[model."<id>/<model>"]` table in `settings` when its ceiling differs
          from its siblings'.

          A `format = "messages"` provider must set this. The Messages API
          requires `max_tokens` on every request and rejects a value above the
          target model's own output limit, which nothing at request-build time
          can look up — so grok refuses to guess one and fails the request
          instead. `null` (the default) omits the field, which is correct for
          the other three formats.
        '';
      };
      auth_account = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          Which of a credential plugin's accounts this provider's models
          authenticate as, threaded to the plugin credential seam as
          `ownerHint`. Lets one sidecar hold several accounts for the same
          provider, so two entries sharing a `base_url` stay distinct
          credentials. `null` (the default) means the plugin's default account.
        '';
      };
      reasoning_efforts = mkOption {
        type = types.listOf reasoningEffortType;
        default = [];
        example = ["low" "medium" "high"];
        description = ''
          Reasoning-effort menu this provider's models offer. A non-empty list
          implies `supports_reasoning_effort` and — absent an explicit
          `reasoning_effort` — supplies the default (its first entry), so this
          is normally the only one of the three you set. Empty (the default)
          omits the field.

          Effort support is a property of the *endpoint*, so every model the
          provider serves inherits this menu; override a single model with a
          `[model."<id>/<model>"]` table in `settings` when its acceptable
          levels differ from its siblings'.
        '';
      };
      reasoning_effort = mkOption {
        type = types.nullOr reasoningEffortType;
        default = null;
        description = ''
          Effort sent when the session has not picked one. Set it to pin a
          default other than the menu's own first entry; `null` (the default)
          derives it from `reasoning_efforts`.
        '';
      };
      supports_reasoning_effort = mkOption {
        type = types.bool;
        default = false;
        description = ''
          Escape hatch for an endpoint that accepts an effort but whose menu you
          do not want to enumerate: exposes the control with the client's
          fallback list. Implied by a non-empty `reasoning_efforts`, so leave it
          `false` (the default) whenever you set that.
        '';
      };
      thinking = mkOption {
        type = types.nullOr thinkingDialectType;
        default = null;
        example = "adaptive";
        description = ''
          Which `thinking` dialect this provider's models accept. Only the
          `messages` format has such a field; the other three ignore it.

          The accepted dialect follows from the model generation the endpoint
          serves and the wrong one is a hard rejection, so it has to be declared
          — nothing downstream can classify an arbitrary slug, since a gateway
          may serve any model under any name. `"adaptive"` for models that let
          the endpoint pick the budget, `{ budget_tokens = 8000; }` for older
          ones that require an explicit budget, `"off"` to send no `thinking`
          field at all.

          Like the effort menu, it describes the *endpoint*, so every model the
          provider serves inherits it; override a single model with a
          `[model."<id>/<model>"]` table in `settings` when its generation
          differs from its siblings'.

          `null` (the default) declares nothing and keeps grok's previous
          behaviour, so an existing provider entry is unaffected.
        '';
      };
      max_concurrent = mkOption {
        type = types.nullOr types.ints.positive;
        default = null;
        example = 4;
        description = ''
          How many requests this provider will serve at once, per model.

          For an endpoint that enforces a hard parallelism limit: over the limit
          it rejects the surplus rather than queueing it, so grok queues locally
          instead and admits at most this many requests per model at a time —
          counting the main turn, subagent turns, and every auxiliary one-shot
          (session title, image description, the Auto-mode classifier,
          `web_fetch` distillation, prompt suggestion). Auxiliary work is held to
          one slot fewer than the limit so a burst of it cannot displace the turn
          you are waiting on.

          Like `context_window` and `max_completion_tokens`, it describes the
          *endpoint*, so every model the provider serves inherits it; override a
          single model with a `[model."<id>/<model>"]` table in `settings` when
          its limit differs from its siblings'.

          `null` (the default) declares no limit and runs no admission control
          at all. Changing the value takes effect on restart.
        '';
      };
    };
  };

  # ProviderConfig applies `skip_serializing_if` to `api_key`, `auth_scheme`,
  # `proxy`, `context_window`, `max_completion_tokens`, `auth_account`,
  # `reasoning_effort`, `thinking`, `max_concurrent` (Option::is_none), to
  # empty `headers`/`models`/`reasoning_efforts`, and to a false
  # `supports_reasoning_effort`. Nix's TOML writer cannot emit `null`, so drop
  # those keys here before generating: an omitted key is exactly what the
  # skipped serialization would have produced, and the round-trip parses back
  # to the same ProviderConfig.
  cleanProvider = p:
    lib.filterAttrs (n: v:
      v
      != null
      && !(n == "headers" && v == {})
      && !(n == "models" && v == [])
      && !(n == "reasoning_efforts" && v == [])
      && !(n == "supports_reasoning_effort" && v == false))
    p;
in {
  options.programs.grok-build = {
    enable = mkEnableOption "Grok Build, SpaceXAI's terminal-based AI coding agent";

    package = mkOption {
      type = types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      defaultText = literalExpression "grok-build.packages.\${system}.default";
      description = "The grok-build (`grok`) package to install.";
    };

    settings = mkOption {
      inherit (tomlFormat) type;
      default = {};
      description = ''
        Grok Build configuration, written to `~/.grok/config.toml` — the
        user-tier config file grok reads
        (`xai_grok_config::loader::load_from_disk`,
        crates/codegen/xai-grok-config/src/loader.rs:83-85, which loads
        `<$GROK_HOME or ~/.grok>/config.toml`). Freeform TOML; see the user
        guide under
        crates/codegen/xai-grok-pager/docs/user-guide/06-configuration.md
        for available keys.

        Note: this always writes to the literal `~/.grok/config.toml`.
        `home.file` can only place files under `$HOME`, so if `$GROK_HOME` is
        overridden in the environment at runtime, grok will read from there
        instead and never see this file.
      '';
    };

    plugins = mkOption {
      type = types.attrsOf types.path;
      default = {};
      example = literalExpression "{ my-plugin = ./plugins/my-plugin; }";
      description = ''
        Plugin directories to deploy into grok's user-scope plugin discovery
        location, `~/.grok/plugins/<name>/` — always-trusted `User` scope,
        highest-priority filesystem source after CLI overrides and project
        dirs
        (`xai_grok_agent::plugins::discovery`,
        crates/codegen/xai-grok-agent/src/plugins/discovery.rs:8-15,229).
        Each attribute name becomes the plugin's directory name under
        `~/.grok/plugins/`; each value is a store path to that plugin's
        directory (its manifest, e.g. a convention-based layout or an
        explicit manifest file, is discovered from there per
        `xai_grok_agent::plugins::manifest::load_manifest`).

        Same `$GROK_HOME` caveat as `settings` applies here.
      '';
    };

    agents = mkOption {
      type = types.attrsOf types.path;
      default = {};
      example = literalExpression "{ reviewer = ./agents/reviewer.md; }";
      description = ''
        Agent definitions to deploy into grok's user-scope agent discovery
        location, `~/.grok/agents/<name>.md` — the user tier of
        `xai_grok_agent::discovery`. Each is a Markdown file with YAML
        frontmatter parsed into an `AgentDefinition`
        (crates/codegen/xai-grok-agent/src/config.rs); the body after the
        frontmatter is the agent's system prompt. Each attribute name becomes
        the file's basename (`<name>.md`); each value is a store path to that
        `.md` file.

        Same `$GROK_HOME` caveat as `settings` applies here.
      '';
    };

    workflows = mkOption {
      type = types.attrsOf types.path;
      default = {};
      example = literalExpression "{ deep-review = ./workflows/deep-review.rhai; }";
      description = ''
        Multi-agent workflow scripts to deploy into grok's user-scope workflow
        discovery location, `~/.grok/workflows/<name>.rhai` — the user tier
        resolved by `user_workflow_dir`
        (crates/codegen/xai-grok-shell/src/session/workflow/registry.rs). Each
        is a Rhai script whose `meta` block declares the workflow's name,
        phases and agents; `/workflows` lists them and renders phase/agent
        progress.

        The attribute name becomes the file's basename (`<name>.rhai`) and
        **must equal the script's own `meta.name`** — the registry validates
        the two against each other (`validate_workflow_filename`) and refuses
        the workflow on a mismatch, so a rename here means renaming in the
        script too.

        Project-scoped workflows (`<repo>/.grok/workflows/`) take precedence
        over these; the same `$GROK_HOME` caveat as `settings` applies.
      '';
    };

    providers = mkOption {
      type = types.listOf providerType;
      default = [];
      example = literalExpression ''
        [
          {
            id = "acme";
            format = "messages";
            base_url = "https://example.test/v1";
            api_key = "{file:/run/secrets/acme_key}";
            models = [ "m-large" "m-small" ];
          }
        ]
      '';
      description = ''
        Custom LLM-provider registry entries, emitted as the `[[provider]]`
        TOML array grok reads (`xai_grok_config_types::provider::ProviderConfig`).
        Each entry declares an external inference endpoint — its wire format,
        base URL, credential, headers, optional proxy — and the model slugs it
        serves; grok synthesizes `<id>/<model>` catalog entries from it.

        Merged into the generated `~/.grok/config.toml` alongside `settings`
        (this option wins on the `provider` key). Put credentials and private
        URLs behind `{file:/path}` references (see `api_key`) rather than
        literals so no secret ends up in the Nix store.
      '';
    };
  };

  config = mkIf cfg.enable {
    home.packages = [cfg.package];

    home.file =
      {
        ".grok/config.toml".source =
          tomlFormat.generate "grok-config.toml"
          (cfg.settings
            // lib.optionalAttrs (cfg.providers != []) {
              provider = map cleanProvider cfg.providers;
            });
      }
      // lib.mapAttrs' pluginFile cfg.plugins
      // lib.mapAttrs' agentFile cfg.agents
      // lib.mapAttrs' workflowFile cfg.workflows;
  };
}
