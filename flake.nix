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

        toolServer = rustPlatform.buildRustPackage (commonArgs // {
          pname = "latchkey-tool-server";
          cargoBuildFlags = [ "-p" "latchkey-tool-server" ];
          cargoTestFlags = [ "-p" "latchkey-tool-server" ];
        });

        upstreamStub = rustPlatform.buildRustPackage (commonArgs // {
          pname = "latchkey-upstream-stub";
          cargoBuildFlags = [ "-p" "latchkey-upstream-stub" ];
          cargoTestFlags = [ "-p" "latchkey-upstream-stub" ];
        });

        gatewayImage = pkgs.dockerTools.buildLayeredImage {
          name = "latchkey-gateway";
          tag = "dev";
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
          tag = "dev";
          contents = [ operator pkgs.cacert ];
          config = {
            Cmd = [ "${operator}/bin/latchkey-operator" ];
            Env = [ "RUST_LOG=info" ];
            User = "65532:65532";
          };
        };

        toolServerImage = pkgs.dockerTools.buildLayeredImage {
          name = "latchkey-tool-server";
          tag = "dev";
          contents = [ toolServer pkgs.cacert ];
          config = {
            Cmd = [ "${toolServer}/bin/latchkey-tool-server" ];
            ExposedPorts = {
              "8081/tcp" = { };
            };
            Env = [ "RUST_LOG=info" ];
            User = "65532:65532";
          };
        };

        upstreamStubImage = pkgs.dockerTools.buildLayeredImage {
          name = "latchkey-upstream-stub";
          tag = "dev";
          contents = [ upstreamStub pkgs.cacert ];
          config = {
            Cmd = [ "${upstreamStub}/bin/latchkey-upstream-stub" ];
            ExposedPorts = {
              "8082/tcp" = { };
            };
            Env = [ "RUST_LOG=info" ];
            User = "65532:65532";
          };
        };

        bootstrapBundle = pkgs.linkFarm "latchkey-bootstrap" [
          {
            name = "gateway";
            path = gateway;
          }
          {
            name = "operator";
            path = operator;
          }
          {
            name = "tool-server";
            path = toolServer;
          }
          {
            name = "upstream-stub";
            path = upstreamStub;
          }
          {
            name = "gateway-image";
            path = gatewayImage;
          }
          {
            name = "operator-image";
            path = operatorImage;
          }
          {
            name = "tool-server-image";
            path = toolServerImage;
          }
          {
            name = "upstream-stub-image";
            path = upstreamStubImage;
          }
        ];
      in {
        packages = {
          inherit gateway operator;
          tool-server = toolServer;
          upstream-stub = upstreamStub;
          gateway-image = gatewayImage;
          operator-image = operatorImage;
          tool-server-image = toolServerImage;
          upstream-stub-image = upstreamStubImage;
          default = bootstrapBundle;
        };

        apps = {
          gateway = flake-utils.lib.mkApp { drv = gateway; };
          operator = flake-utils.lib.mkApp { drv = operator; };
          tool-server = flake-utils.lib.mkApp { drv = toolServer; };
          upstream-stub = flake-utils.lib.mkApp { drv = upstreamStub; };
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
