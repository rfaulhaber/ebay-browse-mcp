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

        packages =
          {
            ${projectName} = pkgs.rustPlatform.buildRustPackage {
              pname = projectName;
              version = cargoToml.package.version;
              src = ./.;
              cargoLock.lockFile = ./Cargo.lock;
            };
            default = self'.packages.${projectName};
          }
          // pkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
            # Streamed rather than built: `./result` is a script writing a
            # docker-archive to stdout, so CI pipes it straight into skopeo
            # and no image tarball enters the store or the workspace.
            oci-image = pkgs.dockerTools.streamLayeredImage {
              name = projectName;
              # Per-commit tags so the registry can roll back between
              # releases; CI retags the newest one as :latest.
              tag = "${cargoToml.package.version}-${self.shortRev or "dirty"}";
              # reqwest probes the system CA store at runtime, and its probe
              # list covers distro paths (ca-certificates.crt and friends)
              # but not cacert's ca-bundle.crt — without both the bundle and
              # SSL_CERT_FILE, every eBay call fails the TLS handshake.
              contents = [pkgs.cacert];
              config = {
                Entrypoint = ["${self'.packages.${projectName}}/bin/${projectName}"];
                # Same runtime user as the Dockerfile; numeric because the
                # image has no /etc/passwd to resolve a name against.
                User = "10001:10001";
                Env = [
                  "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
                  # A detached container wants the remote transport; stdio
                  # remains reachable by overriding this to an empty value,
                  # which Config treats as unset.
                  "EBAY_MCP_HTTP_ADDR=0.0.0.0:8080"
                ];
                ExposedPorts."8080/tcp" = {};
              };
            };
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
