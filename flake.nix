{
  description = "mix, reproducible systems, made effortless";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    crane.url = "github:ipetkov/crane";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
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
      system = "x86_64-linux";
      pkgs = import nixpkgs {
        inherit system;
        overlays = [ (import rust-overlay) ];
      };
      inherit (pkgs) lib;
      craneLib = (crane.mkLib pkgs).overrideToolchain (
        p: p.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml
      );
      src = lib.fileset.toSource {
        root = ./.;
        fileset = lib.fileset.unions [
          ./Cargo.toml
          ./Cargo.lock
          ./clippy.toml
          ./deny
          ./.cargo
          ./crates
        ];
      };
      common = {
        inherit src;
        strictDeps = true;
        CARGO_PROFILE = "";
        cargoExtraArgs = "--locked --workspace";
        pname = "mix";
        version = "0.1.0";
      };
      cargoArtifacts = craneLib.buildDepsOnly common;
      layers = [
        "core"
        "cli"
        "explain"
        "render"
        "ui"
      ];
    in
    {
      packages.${system}.default = craneLib.buildPackage (
        common
        // {
          inherit cargoArtifacts;
          cargoExtraArgs = "--locked --package mix-cli --package mix-daemon";
          doCheck = false;
        }
      );

      checks.${system} = {
        tests = craneLib.cargoTest (
          common
          // {
            inherit cargoArtifacts;
            CI = "true";
            INSTA_UPDATE = "no";
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

        deny = craneLib.cargoDeny (
          common
          // {
            cargoExtraArgs = "";
            cargoDenyChecks = "bans licenses sources";
            cargoDenyExtraArgs = "--config deny/workspace.toml";
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
      )
      // {
        fmt = craneLib.cargoFmt (
          common
          // {
            cargoExtraArgs = "--all";
          }
        );
      };
    };
}
