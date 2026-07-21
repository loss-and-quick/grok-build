_: {
  perSystem = {
    pkgs,
    toolchain,
    ...
  }: let
    inherit (toolchain) nativeTools nativeLibs baseHook;
  in {
    # `nix develop` — pinned Rust toolchain, protoc, bun/node/deno, and the
    # native build deps the workspace's build.rs / vendored-C crates need.
    devShells.default = pkgs.mkShell {
      nativeBuildInputs = nativeTools;
      buildInputs = nativeLibs;
      shellHook = baseHook;
    };
  };
}
