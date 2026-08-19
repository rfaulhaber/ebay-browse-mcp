{
  description = "Rust flake template using rust-overlay and flake-parts.";

  inputs = {
    flake-parts.url = "github:hercules-ci/flake-parts";
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    git-hooks = {
      url = "github:cachix/git-hooks.nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = inputs @ {
    self,
    flake-parts,
    ...
  }: let
    cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
    projectName = cargoToml.package.name;
  in
    flake-parts.lib.mkFlake {inherit inputs;} {
      imports = [inputs.git-hooks.flakeModule];
      flake.overlays.rustOverlay = inputs.rust-overlay.overlays.default;
      systems = [
        "x86_64-linux"
        "aarch64-darwin"
        "aarch64-linux"
      ];

      perSystem = {
        config,
        self',
        inputs',
        pkgs,
        system,
        ...
      }: {
        _module.args.pkgs = import inputs.nixpkgs {
          inherit system;
          overlays = [
            self.overlays.rustOverlay
          ];
        };

        formatter = pkgs.alejandra;

        packages = {
          ${projectName} = pkgs.rustPlatform.buildRustPackage {
            pname = projectName;
            version = cargoToml.package.version;
            src = ./.;
            cargoLock.lockFile = ./Cargo.lock;
          };
          default = self'.packages.${projectName};
        };

        # Hooks run the same toolchain as the dev shell rather than
        # nixpkgs' rustc, so hook results match what cargo/rust-analyzer
        # report while developing.
        pre-commit.settings.hooks = let
          toolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
        in {
          clippy = {
            enable = true;
            packageOverrides = {
              cargo = toolchain;
              clippy = toolchain;
            };
            settings.denyWarnings = true;
          };
          rustfmt = {
            enable = true;
            packageOverrides = {
              cargo = toolchain;
              rustfmt = toolchain;
            };
          };
        };

        devShells.default = pkgs.mkShell {
          shellHook = config.pre-commit.installationScript;
          packages = with pkgs; [
            (rust-bin.fromRustupToolchainFile ./rust-toolchain.toml)
            rust-analyzer
            cargo-nextest
            cargo-release
          ];
        };
      };

      flake = {};
    };
}
