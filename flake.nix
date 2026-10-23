{
  description = "Windlass: MAM acquisition, seeding, and library organisation on NixOS";

  inputs.nixpkgs.url = "https://channels.nixos.org/nixos-26.05/nixexprs.tar.xz";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAllSystems (pkgs: rec {
        windlass = pkgs.callPackage ./nix/package.nix { };
        default = windlass;
      });

      nixosModules.default =
        { lib, pkgs, ... }:
        {
          imports = [ ./nix/module.nix ];
          services.windlass.package = lib.mkDefault self.packages.${pkgs.stdenv.hostPlatform.system}.windlass;
        };

      checks = forAllSystems (pkgs: {
        package = self.packages.${pkgs.stdenv.hostPlatform.system}.windlass;
        vpn = pkgs.testers.runNixOSTest (
          import ./nix/tests/vpn.nix { windlassModule = self.nixosModules.default; }
        );
      });

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = with pkgs; [
            cargo
            rustc
            clippy
            rustfmt
            just
            cargo-llvm-cov
          ];
        };
      });

      formatter = forAllSystems (pkgs: pkgs.nixfmt);
    };
}
