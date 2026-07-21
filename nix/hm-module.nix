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
  };

  config = mkIf cfg.enable {
    home.packages = [cfg.package];

    home.file =
      {
        ".grok/config.toml".source = tomlFormat.generate "grok-config.toml" cfg.settings;
      }
      // lib.mapAttrs' pluginFile cfg.plugins;
  };
}
