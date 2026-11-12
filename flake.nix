{
  description = "Keeps the ISOs on a Ventoy stick up to date";

  inputs.nixpkgs.url = "https://flakehub.com/f/NixOS/nixpkgs/0.1"; # unstable Nixpkgs

  outputs =
    { self, ... }@inputs:

    let
      inherit (inputs.nixpkgs) lib;

      supportedSystems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ];

      forEachSupportedSystem =
        f:
        lib.genAttrs supportedSystems (
          system:
          f {
            inherit system;
            pkgs = import inputs.nixpkgs { inherit system; };
          }
        );

      # Single source of truth for the version: Cargo.toml.
      cargoToml = lib.importTOML ./Cargo.toml;
    in
    {
      packages = forEachSupportedSystem (
        { pkgs, system }:
        let
          ventoy-iso-updater = pkgs.rustPlatform.buildRustPackage {
            pname = cargoToml.package.name;
            inherit (cargoToml.package) version;
            src = ./.;
            cargoLock.lockFile = ./Cargo.lock;
            meta.mainProgram = "ventoy-iso-updater";
          };
        in
        {
          inherit ventoy-iso-updater;
          default = ventoy-iso-updater;
        }
      );

      apps = forEachSupportedSystem (
        { pkgs, system }:
        {
          ventoy-iso-updater = {
            type = "app";
            program = "${self.packages.${system}.ventoy-iso-updater}/bin/ventoy-iso-updater";
          };
          default = self.apps.${system}.ventoy-iso-updater;
        }
      );

      devShells = forEachSupportedSystem (
        { pkgs, system }:
        {
          default = pkgs.mkShell {
            packages = with pkgs; [
              cargo
              rustc
              clippy
              rustfmt
              rust-analyzer
              self.formatter.${system}
            ];
          };
        }
      );

      formatter = forEachSupportedSystem ({ pkgs, ... }: pkgs.nixfmt);
    };
}
