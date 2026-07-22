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
          Credential sent per the format's auth scheme (Bearer / `x-api-key` /
          `x-goog-api-key`). Prefer a `{file:/path}` reference over a literal so
          the secret never lands in the world-readable Nix store. `null` (the
          default) omits the field entirely.
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
    };
  };

  # ProviderConfig applies `skip_serializing_if` to `api_key`, `proxy`,
  # `context_window` (Option::is_none) and to empty `headers`/`models`. Nix's
  # TOML writer cannot emit `null`, so drop those keys here before generating:
  # an omitted key is exactly what the skipped serialization would have
  # produced, and the round-trip parses back to the same ProviderConfig.
  cleanProvider = p:
    lib.filterAttrs (n: v:
      v != null
      && !(n == "headers" && v == {})
      && !(n == "models" && v == []))
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
      type = tomlFormat.type;
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
      // lib.mapAttrs' agentFile cfg.agents;
  };
}
