{
  description = "spacetime-quasar: toolchain and development shell";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    # Used by the package and checks outputs, which a later Subtask adds.
    crane.url = "github:ipetkov/crane";
  };

  outputs =
    {
      nixpkgs,
      rust-overlay,
      ...
    }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-darwin"
      ];
      forAllSystems =
        f:
        nixpkgs.lib.genAttrs systems (
          system:
          f {
            inherit system;
            pkgs = import nixpkgs {
              inherit system;
              overlays = [ rust-overlay.overlays.default ];
            };
          }
        );
    in
    {
      formatter = forAllSystems ({ pkgs, ... }: pkgs.nixfmt);

      devShells = forAllSystems (
        { pkgs, ... }:
        let
          inherit (pkgs) lib stdenv;
          rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
          runtimeLibs = with pkgs; [
            vulkan-loader
            wayland
            libxkbcommon
            libx11
            libxcursor
            libxi
            libxrandr
          ];
        in
        {
          default = pkgs.mkShell (
            {
              packages = [
                rustToolchain
                pkgs.sccache
              ]
              ++ lib.optionals stdenv.hostPlatform.isLinux [ pkgs.mold ];

              RUSTC_WRAPPER = "sccache";
              SCCACHE_CACHE_SIZE = "20G";
            }
            // lib.optionalAttrs stdenv.hostPlatform.isLinux {
              # Target-specific, so config-file rustflags are joined, not replaced
              # (never use RUSTFLAGS here). See ADR 0004.
              CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS = "-C linker-features=-lld -C link-arg=-fuse-ld=mold";
              LD_LIBRARY_PATH = lib.makeLibraryPath runtimeLibs;
            }
          );
        }
      );
    };
}
