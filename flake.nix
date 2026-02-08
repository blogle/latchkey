{
  description = "Latchkey MCP gateway and operator bootstrap";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
        rustPlatform = pkgs.rustPlatform;

        commonArgs = {
          version = "0.1.0";
          src = ./.;
          cargoLock = {
            lockFile = ./Cargo.lock;
          };
        };

        gateway = rustPlatform.buildRustPackage (commonArgs // {
          pname = "latchkey-gateway";
          cargoBuildFlags = [ "-p" "latchkey-gateway" ];
          cargoTestFlags = [ "-p" "latchkey-gateway" ];
        });

        operator = rustPlatform.buildRustPackage (commonArgs // {
          pname = "latchkey-operator";
          cargoBuildFlags = [ "-p" "latchkey-operator" ];
          cargoTestFlags = [ "-p" "latchkey-operator" ];
        });

        gatewayImage = pkgs.dockerTools.buildLayeredImage {
          name = "latchkey-gateway";
          tag = "latest";
          contents = [ gateway pkgs.cacert ];
          config = {
            Cmd = [ "${gateway}/bin/latchkey-gateway" ];
            ExposedPorts = {
              "8080/tcp" = { };
            };
            Env = [ "RUST_LOG=info" ];
            User = "65532:65532";
          };
        };

        operatorImage = pkgs.dockerTools.buildLayeredImage {
          name = "latchkey-operator";
          tag = "latest";
          contents = [ operator pkgs.cacert ];
          config = {
            Cmd = [ "${operator}/bin/latchkey-operator" ];
            Env = [ "RUST_LOG=info" ];
            User = "65532:65532";
          };
        };
      in {
        packages = {
          inherit gateway operator;
          gateway-image = gatewayImage;
          operator-image = operatorImage;
          default = gateway;
        };

        apps = {
          gateway = flake-utils.lib.mkApp { drv = gateway; };
          operator = flake-utils.lib.mkApp { drv = operator; };
        };

        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            cargo
            rustc
            clippy
            rustfmt
            rust-analyzer
            cargo-deny
            just
            kubectl
            kustomize
            kind
          ];
        };
      });
}
