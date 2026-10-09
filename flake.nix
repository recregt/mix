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
          ./.cargo
          ./crates
        ];
      };
      common = {
        inherit src;
        strictDeps = true;
        CARGO_PROFILE = "";
        pname = "mix";
        version = "0.1.0";
      };
      cargoArtifacts = craneLib.buildDepsOnly (common // { cargoExtraArgs = "--workspace"; });
    in
    {
      checks.${system}.tests = craneLib.cargoTest (
        common
        // {
          inherit cargoArtifacts;
          cargoTestExtraArgs = "--workspace";
          CI = "true";
          INSTA_UPDATE = "no";
          SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
        }
      );
    };
}
