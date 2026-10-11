{
  description = "spacetime-quasar: game package, checks and development shell";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    # Builds the package and the checks (ADR 0001).
    crane.url = "github:ipetkov/crane";
  };

  outputs =
    {
      nixpkgs,
      rust-overlay,
      crane,
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

      # Libraries that winit and wgpu dlopen at run time (ADR 0001, ADR 0005).
      # Used on Linux only: the darwin package does not reference them.
      runtimeLibsFor =
        pkgs: with pkgs; [
          vulkan-loader
          wayland
          libxkbcommon
          libx11
          libxcursor
          libxi
          libxrandr
        ];

      # Per-system build setup shared by the package and the checks.
      buildFor =
        pkgs:
        let
          inherit (pkgs) lib;
          rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
          craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;
          # Cargo sources plus the config files the checks read, and `.ron` data files
          # (ADR 0010: `assets/**/*.ron`, `crates/*/tests/data/**/*.ron`) that tests read
          # with `include_str!` or from `assets/`. A suffix predicate keeps the set
          # minimal: docs and other files stay out, so editing them does not rebuild.
          src = lib.cleanSourceWith {
            src = ./.;
            filter =
              path: type:
              (craneLib.filterCargoSources path type)
              || lib.hasSuffix ".ron" path
              || builtins.elem (baseNameOf path) [
                "deny.toml"
                "rustfmt.toml"
                # Expected determinism-harness output, read with `include_str!`.
                "reference.txt"
                # The SIM_VERSION pin (ADR 0009), read with `include_str!`.
                "sim-version.txt"
              ];
            name = "source";
          };
          common = {
            inherit src;
            strictDeps = true;
            pname = "spacetime-quasar";
            version = "0.0.0";
          };
          cargoArtifacts = craneLib.buildDepsOnly common;
        in
        {
          inherit
            lib
            craneLib
            common
            cargoArtifacts
            ;
        };
    in
    {
      formatter = forAllSystems ({ pkgs, ... }: pkgs.nixfmt);

      packages = forAllSystems (
        { pkgs, ... }:
        let
          inherit (buildFor pkgs)
            lib
            craneLib
            common
            cargoArtifacts
            ;
          isLinux = pkgs.stdenv.hostPlatform.isLinux;
          rpath = lib.makeLibraryPath (runtimeLibsFor pkgs);
          game = craneLib.buildPackage (
            common
            // {
              inherit cargoArtifacts;
              cargoExtraArgs = "--locked -p spacetime-quasar";
              # The checks run the tests.
              doCheck = false;
              nativeBuildInputs = lib.optionals isLinux [ pkgs.patchelf ];
              # winit and wgpu dlopen these libraries; they are not DT_NEEDED
              # entries. glibc searches the caller's RUNPATH for dlopen, so no
              # LD_LIBRARY_PATH is needed (ADR 0001).
              postFixup = lib.optionalString isLinux ''
                patchelf --add-rpath ${rpath} $out/bin/spacetime-quasar
              '';
              meta.mainProgram = "spacetime-quasar";
            }
          );
        in
        {
          default = game;
        }
      );

      checks = forAllSystems (
        { pkgs, system, ... }:
        let
          inherit (buildFor pkgs)
            lib
            craneLib
            common
            cargoArtifacts
            ;
          # Every AsciiDoc file, for the documentation check.
          docsSrc = lib.fileset.toSource {
            root = ./.;
            fileset = lib.fileset.fileFilter (file: file.hasExt "adoc") ./.;
          };
          wasmArgs = common // {
            CARGO_BUILD_TARGET = "wasm32-unknown-unknown";
            doCheck = false;
          };
        in
        {
          fmt = craneLib.cargoFmt { inherit (common) src pname version; };

          nixfmt = pkgs.runCommand "nixfmt-check" { nativeBuildInputs = [ pkgs.nixfmt ]; } ''
            nixfmt --check ${./flake.nix}
            touch $out
          '';

          clippy = craneLib.cargoClippy (
            common
            // {
              inherit cargoArtifacts;
              cargoClippyExtraArgs = "--workspace --all-targets -- -D warnings";
            }
          );

          test = craneLib.cargoTest (
            common
            // {
              inherit cargoArtifacts;
              cargoTestExtraArgs = "--workspace";
            }
          );

          bench-build = craneLib.mkCargoDerivation (
            common
            // {
              inherit cargoArtifacts;
              pnameSuffix = "-bench-build";
              buildPhaseCargoCommand = "cargo bench --workspace --no-run --locked";
              doInstallCargoArtifacts = false;
              installPhaseCommand = "touch $out";
            }
          );

          doc = craneLib.cargoDoc (
            common
            // {
              inherit cargoArtifacts;
              cargoDocExtraArgs = "--workspace --no-deps";
              RUSTDOCFLAGS = "-D warnings";
            }
          );

          asciidoc = pkgs.runCommand "asciidoc-check" { nativeBuildInputs = [ pkgs.asciidoctor ]; } ''
            cd ${docsSrc}
            find . -name '*.adoc' -print0 \
              | xargs -0 -n1 asciidoctor --failure-level WARN -o /dev/null
            touch $out
          '';

          # Bans, licences and sources only: advisories need the network,
          # which the sandbox lacks (ADR 0003). The scheduled workflow runs
          # advisories.
          deny = craneLib.cargoDeny {
            inherit (common) src pname version;
            cargoDenyChecks = "bans licenses sources";
          };

          # ADR 0002: engine crates are also rejected by prefix in the lockfile.
          lockfile-prefix = pkgs.runCommand "lockfile-prefix-check" { } ''
            if grep -E '^name = "(bevy|godot|gdnative|fyrox|rg3d|three-d|rend3)([-_][a-z0-9_-]*)?"$' ${./Cargo.lock}; then
              echo "banned engine crate in Cargo.lock"
              exit 1
            fi
            touch $out
          '';
        }
        // lib.optionalAttrs (system == "x86_64-linux") {
          wasm32 = craneLib.cargoBuild (
            wasmArgs
            // {
              pnameSuffix = "-wasm32";
              cargoArtifacts = craneLib.buildDepsOnly wasmArgs;
              cargoExtraArgs = "--workspace --locked";
            }
          );
        }
      );

      devShells = forAllSystems (
        { pkgs, ... }:
        let
          inherit (pkgs) lib stdenv;
          rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
          runtimeLibs = runtimeLibsFor pkgs;
        in
        {
          default = pkgs.mkShell (
            {
              packages = [
                rustToolchain
                pkgs.sccache
                pkgs.cargo-deny
                # Runs the wasm32-wasip1 tests and the determinism harness (ADR 0008).
                pkgs.wasmtime
                # Renders and checks the AsciiDoc docs (docs rules in AGENTS.md).
                pkgs.asciidoctor
              ]
              ++ lib.optionals stdenv.hostPlatform.isLinux [ pkgs.mold ];

              RUSTC_WRAPPER = "sccache";
              SCCACHE_CACHE_SIZE = "20G";
              # `cargo test --target wasm32-wasip1` runs through wasmtime; the
              # current directory is the only directory the module may open.
              CARGO_TARGET_WASM32_WASIP1_RUNNER = "wasmtime run --dir=.";
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
