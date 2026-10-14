{
  description = "A Nix-flake-based Python development environment";

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

      pythonVersion = "3.13";
    in
    {
      packages = forEachSupportedSystem (
        { pkgs, system }:
        let
          concatMajorMinor =
            v:
            lib.pipe v [
              lib.versions.splitVersion
              (lib.sublist 0 2)
              lib.concatStrings
            ];

          python = pkgs."python${concatMajorMinor pythonVersion}";

          ventoy-iso-updater = pkgs.stdenvNoCC.mkDerivation {
            pname = "ventoy-iso-updater";
            version = "v0.9.0";

            src = ./.;

            nativeBuildInputs = [ pkgs.makeWrapper ];

            dontBuild = true;
            dontConfigure = true;

            installPhase = ''
              install -Dm755 update_ventoy_isos.py $out/bin/ventoy-iso-updater
              wrapProgram $out/bin/ventoy-iso-updater \
                --argv0 ventoy-iso-updater \
                --prefix PATH : ${lib.makeBinPath [ python ]}
            '';
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
        let
          concatMajorMinor =
            v:
            lib.pipe v [
              lib.versions.splitVersion
              (lib.sublist 0 2)
              lib.concatStrings
            ];

          python = pkgs."python${concatMajorMinor pythonVersion}";
        in
        {
          default = pkgs.mkShellNoCC {
            venvDir = ".venv";

            postShellHook = ''
              venvVersionWarn() {
              	local venvVersion
              	venvVersion="$("$venvDir/bin/python" -c 'import platform; print(platform.python_version())')"

              	[[ "$venvVersion" == "${python.version}" ]] && return

              	cat <<EOF
              Warning: Python version mismatch: [$venvVersion (venv)] != [${python.version}]
                       Delete '$venvDir' and reload to rebuild for version ${python.version}
              EOF
              }

              venvVersionWarn
            '';

            packages =
              (with python.pkgs; [
                venvShellHook
                pip
              ])
              ++ [ self.formatter.${system} ];
          };
        }
      );

      formatter = forEachSupportedSystem ({ pkgs, ... }: pkgs.nixfmt);
    };
}
