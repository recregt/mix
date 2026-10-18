{
  description = "mix, reproducible systems, made effortless";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    crane.url = "github:ipetkov/crane";
    rust-overlay = {
      url = "github:oxalica/rust-overlay/87ffb7f777aef2e2647fd8fd718a5b3cbc825588";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      nixpkgs,
      crane,
      rust-overlay,
      ...
    }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      outputsFor =
        system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ (import rust-overlay) ];
          };
          inherit (pkgs) lib;
          version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;
          craneLib = (crane.mkLib pkgs.pkgsStatic).overrideToolchain (
            p: p.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml
          );
          src = lib.fileset.toSource {
            root = ./.;
            fileset = lib.fileset.unions [
              ./Cargo.toml
              ./Cargo.lock
              ./clippy.toml
              ./deny
              ./crates
            ];
          };
          common = {
            inherit src;
            strictDeps = true;
            nativeBuildInputs = [ pkgs.mold ];
            CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_RUSTFLAGS = "-C link-arg=-fuse-ld=mold -C link-arg=-Wl,--no-dynamic-linker";
            CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_RUSTFLAGS = "-C link-arg=-fuse-ld=mold -C link-arg=-Wl,--no-dynamic-linker";
            CARGO_PROFILE = "release";
            cargoExtraArgs = "--locked --workspace";
            pname = "mix";
            inherit version;
          };
          cargoArtifacts = craneLib.buildDepsOnly common;
          distArtifacts = craneLib.buildDepsOnly (common // { CARGO_PROFILE = "dist"; });
          mix = craneLib.buildPackage (
            common
            // {
              CARGO_PROFILE = "dist";
              cargoArtifacts = distArtifacts;
              preBuild = ''
                export CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_RUSTFLAGS="$CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_RUSTFLAGS --remap-path-prefix=$NIX_BUILD_TOP=/build"
                export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_RUSTFLAGS="$CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_RUSTFLAGS --remap-path-prefix=$NIX_BUILD_TOP=/build"
              '';
              cargoExtraArgs = "--locked --package mix-cli --package mix-daemon";
              doCheck = false;
            }
          );
          fuzzManifests = [
            "crates/nixgen/fuzz"
            "crates/render/fuzz"
          ];
          codspeed = lib.findFirst (package: package.name == "codspeed") (throw "Cargo.lock has no codspeed") (
            builtins.fromTOML (builtins.readFile ./Cargo.lock)
          ).package;
          cargoCodspeed = pkgs.cargo-codspeed.overrideAttrs (
            final: previous: {
              inherit (codspeed) version;
              src = previous.src.override {
                tag = "v${codspeed.version}";
                hash = "sha256-M2dqibNz7tiiEu2ioiOIyOgNfoS8xlZy11vqUU2pKRU=";
              };
              cargoDeps = pkgs.rustPlatform.fetchCargoVendor {
                inherit (final) src;
                hash = "sha256-NLu5je+8JDEy+jMg8FotuQu6THVH49Sb1Xd4488FmAo=";
              };
            }
          );
          nightly = pkgs.rust-bin.selectLatestNightlyWith (
            toolchain: toolchain.minimal.override { extensions = [ "llvm-tools-preview" ]; }
          );
          layers = [
            "core"
            "cli"
            "explain"
            "fs"
            "home"
            "render"
            "ui"
          ];
        in
        {
          devShells.default = pkgs.mkShell {
            packages = [ (pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml) ];
          };

          devShells.bench = pkgs.mkShell {
            packages = [
              (pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml)
              cargoCodspeed
            ];
          };

          devShells.fuzz = pkgs.mkShell {
            packages = [
              nightly
              pkgs.cargo-fuzz
              pkgs.tmux
            ];
          };

          packages.default = mix;

          packages.release =
            pkgs.runCommand "mix-${version}-${system}"
              {
                nativeBuildInputs = [
                  pkgs.gnutar
                  pkgs.gzip
                ];
              }
              ''
                name=mix-${system}
                mkdir -p $name $out
                cp ${mix}/bin/mix ${mix}/bin/mix-daemon $name/
                cp ${./LICENSE} $name/LICENSE
                tar --sort=name --mtime=@0 --owner=0 --group=0 --numeric-owner -cf - $name | gzip -n > $out/$name.tar.gz
                cd $out
                sha256sum $name.tar.gz > $name.tar.gz.sha256
              '';

          checks = {
            tests = craneLib.cargoNextest (
              common
              // {
                inherit cargoArtifacts;
                cargoNextestExtraArgs = "--run-ignored all";
                CI = "true";
                SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
              }
            );

            clippy = craneLib.cargoClippy (
              common
              // {
                inherit cargoArtifacts;
                cargoClippyExtraArgs = "--all-targets -- -D warnings";
              }
            );

            doc = craneLib.cargoDoc (
              common
              // {
                inherit cargoArtifacts;
                cargoDocExtraArgs = "--no-deps --document-private-items";
                RUSTDOCFLAGS = "-D warnings";
              }
            );

            ruff =
              pkgs.runCommand "mix-ruff"
                {
                  nativeBuildInputs = [ pkgs.ruff ];
                  root = lib.fileset.toSource {
                    root = ./.;
                    fileset = lib.fileset.unions [
                      ./e2e
                      ./ruff.toml
                    ];
                  };
                  RUFF_NO_CACHE = "true";
                }
                ''
                  cd $root
                  ruff check .
                  ruff format --check .
                  touch $out
                '';

            machete = pkgs.runCommand "mix-machete" { nativeBuildInputs = [ pkgs.cargo-machete ]; } ''
              cd ${src}
              cargo-machete
              touch $out
            '';

            buf-lint =
              pkgs.runCommand "mix-buf-lint"
                {
                  nativeBuildInputs = [ pkgs.buf ];
                  protos = lib.fileset.toSource {
                    root = ./crates;
                    fileset = lib.fileset.unions [
                      ./crates/events/proto
                      ./crates/rpc/proto
                    ];
                  };
                }
                ''
                  export HOME=$TMPDIR
                  buf lint $protos/events/proto
                  buf lint $protos/rpc/proto
                  touch $out
                '';

            fuzz = craneLib.mkCargoDerivation (
              common
              // {
                pname = "mix-fuzz";
                cargoArtifacts = null;
                cargoVendorDir = craneLib.vendorMultipleCargoDeps {
                  cargoLockList = map (dir: ./. + "/${dir}/Cargo.lock") fuzzManifests;
                };
                buildPhaseCargoCommand = lib.concatMapStrings (dir: ''
                  cargo check --locked --all-targets --manifest-path ${dir}/Cargo.toml
                '') fuzzManifests;
                doInstallCargoArtifacts = false;
                installPhaseCommand = "touch $out";
              }
            );

            deny = craneLib.cargoDeny (
              common
              // {
                cargoExtraArgs = "";
                cargoDenyChecks = "bans licenses sources";
                cargoDenyExtraArgs = "--config deny/workspace.toml";
              }
            );

            fmt = craneLib.cargoFmt (
              common
              // {
                cargoExtraArgs = "--all";
              }
            );
          }
          // lib.listToAttrs (
            map (layer: {
              name = "deny-${layer}";
              value = craneLib.cargoDeny (
                common
                // {
                  pname = "mix-${layer}";
                  cargoExtraArgs = "";
                  cargoDenyChecks = "bans";
                  cargoDenyExtraArgs = "--log-level error --manifest-path crates/${layer}/Cargo.toml --config deny/${layer}.toml";
                }
              );
            }) layers
          );
        };
      perSystem = nixpkgs.lib.genAttrs systems outputsFor;
    in
    {
      packages = nixpkgs.lib.mapAttrs (_: outputs: {
        inherit (outputs.packages) default release;
      }) perSystem;

      checks = nixpkgs.lib.mapAttrs (_: outputs: outputs.checks) perSystem;

      devShells = nixpkgs.lib.mapAttrs (_: outputs: outputs.devShells) perSystem;
    };
}
