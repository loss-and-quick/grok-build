# Shared toolchain + native-dependency set derived from `pkgs` (rust-overlay
# overlay applied in flake.nix). Returned attrset is threaded into nix/shells.nix.
{pkgs}: let
  inherit (pkgs) lib;

  # The pinned toolchain is single-sourced from //rust-toolchain.toml (also read by
  # rustup for a bare `cargo`), so the version lives in exactly one place. Bump the
  # file to move the dev shell and `nix flake check` together.
  #
  # `rust-src` is added on top of the file's `[rustfmt, clippy]` components so
  # rust-analyzer (added separately below, since it's not a rustup component in
  # the `default` profile) can resolve std/core sources.
  rustToolchain =
    (pkgs.rust-bin.fromRustupToolchainFile ../rust-toolchain.toml).override
    (old: {
      extensions = (old.extensions or []) ++ ["rust-src"];
    });

  # rustc/cargo/rustfmt/clippy/rust-src for the host triple, plus rust-analyzer
  # (a separate nixpkgs package — not part of rustup's `default` profile, so it
  # tracks nixpkgs rather than the pinned toolchain file; that's fine, the LSP
  # server version is decoupled from the compiler version it drives).
  rustTools = [
    rustToolchain
    pkgs.rust-analyzer
  ];

  # Bun-first, with node and deno alongside: the in-progress TS plugin system
  # discovers a JS runtime in that order (bun -> node -> deno) at plugin-load
  # time, so all three need to be on PATH for local dev/testing of that
  # fallback chain, not just the primary one.
  nodeTools = with pkgs; [
    bun
    nodejs_22
    deno
  ];

  cliTools = with pkgs; [
    git
    jq
  ];

  # Native deps actually exercised by the workspace's build.rs / vendored-C
  # crates (checked against Cargo.lock, not cargo-culted):
  #   - protobuf: crates/build/xai-proto-build compiles crates/*/proto/*.proto.
  #     find_protoc() (crates/build/xai-proto-build/src/find_protoc.rs) checks
  #     $PROTOC first, so pointing PROTOC at nixpkgs' protoc (below) skips the
  #     bin/protoc DotSlash wrapper and its network fetch entirely.
  #   - cmake: aws-lc-sys (pulled in via tonic's "tls-aws-lc" feature) shells
  #     out to cmake to build AWS-LC.
  #   - pkg-config + zlib: libz-sys (a libgit2-sys dependency) and zstd-sys
  #     probe for a system lib via pkg-config before falling back to vendoring;
  #     giving them a system zlib to find avoids an extra vendored build.
  #   - a C compiler: needed by zstd-sys, libz-sys, libgit2-sys ("vendored-libgit2"
  #     git2 feature, used everywhere git2 appears in this workspace),
  #     libsqlite3-sys ("bundled" rusqlite feature, used by every rusqlite
  #     consumer here), ring, aws-lc-sys and tikv-jemalloc-sys. `mkShell`
  #     already puts `stdenv.cc` on nativeBuildInputs, so no explicit gcc entry
  #     is needed.
  #
  # Deliberately NOT included: openssl (reqwest/tonic use rustls; no
  # openssl-sys anywhere in Cargo.lock), sqlite/libgit2 system libraries
  # (everything above is vendored/bundled, not linked), ALSA/audio libs (cpal
  # is target-gated off Linux in xai-grok-voice's Cargo.toml — the Linux build
  # shells out to a system recorder instead), X11/Wayland dev headers (arboard
  # + wl-clipboard-rs use dlopen'd clients, not link-time system libs).
  nativeTools = with pkgs;
    [
      pkg-config
      cmake
      protobuf
    ]
    ++ rustTools
    ++ nodeTools
    ++ cliTools;

  nativeLibs = with pkgs; [
    zlib
  ];

  baseHook = ''
    export PROJECT_ROOT="''${PROJECT_ROOT:-$PWD}"
    export PROTOC="${pkgs.protobuf}/bin/protoc"
    export RUST_SRC_PATH="${rustToolchain}/lib/rustlib/src/rust/library"

    # tikv-jemalloc-sys (pulled in by the pager binary crates) vendors and
    # `./configure`s its own jemalloc at Cargo's chosen optimization level. In
    # a `dev`/`cargo check` profile that's -O0, which collides with glibc's
    # `_FORTIFY_SOURCE` (implied by the gcc-wrapper's default "fortify"
    # hardening flag): glibc's features.h turns "_FORTIFY_SOURCE requires
    # compiling with optimization (-O)" into a hard `-Werror` failure,
    # aborting jemalloc's `configure` with "cannot determine return type of
    # strerror_r". Dropping just the fortify flags (keeping the rest of the
    # wrapper's hardening) matches upstream jemalloc's own recommendation for
    # -O0 builds and is scoped to this shell only — it does not affect how
    # the workspace's own binaries get linked/hardened for release.
    export NIX_HARDENING_ENABLE="$(echo "$NIX_HARDENING_ENABLE" | ${pkgs.gnused}/bin/sed -E 's/\bfortify3?\b//g')"
  '';
in {
  # `rustToolchain` is exposed on its own (not just folded into `nativeTools`)
  # so nix/package.nix can hand it to crane's `overrideToolchain` — the release
  # build needs the bare toolchain derivation, not the devShell's full tool list
  # (rust-analyzer, bun/node/deno, git, jq).
  inherit lib nativeTools nativeLibs baseHook rustToolchain;
}
