{
  description = "x4-cli: put books on an Xteink X4 over Wi-Fi";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
      cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
    in
    {
      packages = forAllSystems (pkgs: {
        default = pkgs.rustPlatform.buildRustPackage {
          pname = "x4-cli";
          version = cargoToml.package.version;
          src = pkgs.lib.cleanSourceWith {
            src = ./.;
            filter = path: type: baseNameOf path != "target";
          };
          cargoLock.lockFile = ./Cargo.lock;
          meta.mainProgram = "x4-cli";
        };
      });

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = [ pkgs.cargo pkgs.rustc pkgs.clippy pkgs.rustfmt pkgs.rust-analyzer ];
        };
      });
    };
}
