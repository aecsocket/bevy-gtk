{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    nixgl.url = "github:nix-community/nixGL";
  };
  outputs = {
    nixpkgs,
    flake-utils,
    rust-overlay,
    nixgl,
    ...
  }: flake-utils.lib.eachDefaultSystem (system:
    let
      pkgs = import nixpkgs {
        inherit system;
        overlays = [
          (import rust-overlay)
          nixgl.overlay
        ];
      };
      lib = pkgs.lib;
      rustToolchain = pkgs.pkgsBuildHost.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
    in {
      devShells.default = pkgs.mkShell {
        buildInputs = with pkgs; [
          just
          typos

          # Nix
          nil
          nixfmt-rfc-style

          # Rust
          rustToolchain
          taplo
          cargo-shear
          pkg-config

          # GTK + Adwaita
          gtk4
          libadwaita
          blueprint-compiler
        ] ++ lib.optionals (lib.strings.hasInfix "linux" system) [
          # Bevy
          alsa-lib
          vulkan-loader
          vulkan-tools
          libudev-zero
          libxkbcommon
        ];
        shellHook = ''
          export RUSTFLAGS="-Zcodegen-backend=cranelift"
        '';
      };
    }
  );
}
