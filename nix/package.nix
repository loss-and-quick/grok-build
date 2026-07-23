# The `xai-grok-pager` binary — the composition-root crate for the Grok
# Build TUI (crates/codegen/xai-grok-pager-bin; ships as `grok` in official
# installs — see README.md's "Building from source"). Built via crane rather
# than rustPlatform.buildRustPackage, matching Magic_V2Ray's crane-based Rust
# packaging (nix/desktop.nix there), the user's declared reference for
# "correct" packaging in this fleet of flakes: crane vendors Cargo.lock
# directly, including its two git-source deps (nucleo / nucleo-matcher, from
# github:helix-editor/nucleo), with no cargoLock.outputHashes bookkeeping the
# way rustPlatform.buildRustPackage would need.
#
# `craneLib` (threaded in via flake.nix's `_module.args`) is already
# `overrideToolchain`d onto this repo's pinned toolchain (nix/toolchain.nix,
# rust-toolchain.toml: 1.92.0, edition 2024) — unlike Magic_V2Ray's own
# craneLib, which floats on nixpkgs' default rustc.
_: {
  perSystem = {
    pkgs,
    toolchain,
    craneLib,
    ...
  }: let
    inherit (toolchain) nativeLibs;

    # The raw flake source, NOT `craneLib.cleanCargoSource`: this workspace's
    # xai-proto-build compiles crates/*/proto/*.proto from build.rs, and the
    # tree also carries non-.rs build inputs (prompt .txt files, vendored
    # mermaid assets) that cleanCargoSource's .rs/Cargo.*-only filter would
    # strip, breaking codegen. Under flakes, `../.` already excludes
    # gitignored `target/`, so the unfiltered tree is both correct and cheap.
    src = ../.;

    commonArgs = {
      inherit src;
      strictDeps = true;

      # Only the composition-root binary and its dependency closure — not
      # `--workspace`, which would also try to build the unrelated dev-only
      # bins that live in sibling crates (bench/probe/playground binaries).
      # Those aren't part of `xai-grok-pager`'s closure and may be mid-edit
      # independently of this package.
      cargoExtraArgs = "--locked -p xai-grok-pager-bin --bin xai-grok-pager";

      # Tests run in CI/devshell (see README.md's Development section); a
      # 79-crate `cargo test` inside a nix build is prohibitive and would
      # duplicate coverage already gated elsewhere.
      doCheck = false;

      # Same native-dependency set the dev shell gives the workspace's
      # build.rs / vendored-C crates (nix/toolchain.nix has the full
      # rationale per-dependency); rust-analyzer/bun/node/deno/git/jq are
      # devShell-only conveniences the release build doesn't need.
      nativeBuildInputs = with pkgs; [pkg-config cmake protobuf];
      buildInputs = nativeLibs; # zlib

      env = {
        # find_protoc() (crates/build/xai-proto-build/src/find_protoc.rs) checks
        # $PROTOC first, so this skips the bin/protoc DotSlash wrapper (and its
        # network fetch) entirely — same reasoning as nix/shells.nix.
        PROTOC = "${pkgs.protobuf}/bin/protoc";

        # xai-grok-tools AND xai-grok-shell each have a build.rs that bundles
        # ripgrep (embedded + self-extracted at runtime); in a release build
        # they DOWNLOAD the musl asset from GitHub unless pointed at a local rg
        # — which the sandboxed (network-free) nix build cannot do. Hand both
        # nixpkgs' rg so the build stays offline. (bfs/ugrep only bundle when
        # their own *_PATH is set, so they no-op.)
        GROK_TOOLS_BUNDLE_RG_PATH = "${pkgs.ripgrep}/bin/rg";
        GROK_SHELL_BUNDLE_RG_PATH = "${pkgs.ripgrep}/bin/rg";
      };
    };

    grok-build = craneLib.buildPackage (commonArgs
      // {
        pname = "grok-build";
        version =
          (craneLib.crateNameFromCargoToml {
            cargoToml = ../crates/codegen/xai-grok-pager-bin/Cargo.toml;
          })
          .version;

        # Official installs ship the `xai-grok-pager` artifact as `grok`
        # (README.md); alias it here so `nix build .#grok` / `nix run
        # .#grok` and `programs.grok-build`'s `getExe` all resolve to the
        # name users actually expect.
        postInstall = ''
          ln -s xai-grok-pager "$out/bin/grok"
        '';

        meta = {
          description = "Grok Build (grok) — SpaceXAI's terminal-based AI coding agent";
          homepage = "https://x.ai/cli";
          license = pkgs.lib.licenses.asl20;
          mainProgram = "grok";
        };
      });
  in {
    packages = {
      default = grok-build;
      grok = grok-build;
    };
  };
}
