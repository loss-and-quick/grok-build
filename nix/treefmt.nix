{
  inputs,
  self,
  ...
}: {
  imports = [
    inputs.treefmt-nix.flakeModule
  ];

  perSystem = _: {
    treefmt.config = {
      projectRootFile = "flake.nix";
      settings = {
        global.excludes = [
          "LICENSE"
          "THIRD-PARTY-NOTICES"
          "SOURCE_REV"

          # Build artifacts (mirrors .gitignore).
          "target/**"
          "result"
          "*.lock"
        ];
      };

      programs = {
        deadnix.enable = true;
        alejandra.enable = true;
        statix.enable = true;
        rustfmt = {
          enable = true;
          # Format with the workspace's own edition rather than treefmt-nix's
          # default, so the treefmt gate and `cargo fmt` (which reads
          # Cargo.toml) can never diverge on an edition bump. Single-sourced
          # from the root Cargo.toml.
          edition =
            (builtins.fromTOML
              (builtins.readFile "${self}/Cargo.toml"))
            .workspace
            .package
            .edition;
        };
      };
    };
  };
}
