{
  description = "Grok Build — SpaceXAI's terminal-based AI coding agent (Rust workspace + upcoming TS plugin runtime)";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable-small";
    flake-parts.url = "github:hercules-ci/flake-parts";
    treefmt-nix.url = "github:numtide/treefmt-nix";
    rust-overlay.url = "github:oxalica/rust-overlay";
    rust-overlay.inputs.nixpkgs.follows = "nixpkgs";
    # Builds the `xai-grok-pager` package (nix/package.nix). Vendors
    # Cargo.lock directly — including its two git-source deps (nucleo /
    # nucleo-matcher) — with no cargoLock.outputHashes bookkeeping needed.
    # Matches Magic_V2Ray's crane-based Rust packaging (nix/desktop.nix
    # there), the user's declared reference for "correct" packaging in this
    # fleet of flakes. No nixpkgs follows, mirroring that reference exactly.
    crane.url = "github:ipetkov/crane";
  };

  # The logic lives in nix/ (one file per concern); this flake just wires it up.
  #
  # perSystem → system-dependent outputs (packages/devShells/formatter).
  # flake     → system-independent outputs (Home Manager module).
  outputs = {
    self,
    flake-parts,
    ...
  } @ inputs: let
    flakeOutputs = flake-parts.lib.mkFlake {inherit inputs;} {
      systems = ["x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin"];

      perSystem = {system, ...}: let
        pkgs = import inputs.nixpkgs {
          inherit system;
          overlays = [inputs.rust-overlay.overlays.default];
        };
        toolchain = import ./nix/toolchain.nix {inherit pkgs;};
        # Pin crane to the workspace's rust-toolchain.toml (1.92.0, edition
        # 2024) rather than nixpkgs' default rustc, so the Nix build and a
        # plain `cargo build` always agree on compiler version.
        craneLib = (inputs.crane.mkLib pkgs).overrideToolchain toolchain.rustToolchain;
      in {
        _module.args = {
          inherit pkgs toolchain craneLib;
        };
      };

      imports = [
        ./nix/shells.nix
        ./nix/treefmt.nix
        ./nix/package.nix
      ];

      flake = {
        # `programs.grok-build.enable = true;` — see nix/hm-module.nix.
        homeManagerModules.default = import ./nix/hm-module.nix {inherit self;};
      };
    };
  in
    flakeOutputs;
}
