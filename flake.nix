{
  description = "alogeno: self-hosted website builder that works with an existing AI subscription";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forEach = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forEach (pkgs: rec {
        alogeno = pkgs.callPackage ./nix/package.nix { };
        default = alogeno;
      });

      overlays.default = final: _prev: {
        alogeno = final.callPackage ./nix/package.nix { };
      };

      # services.alogeno: the default installation and the named ones.
      nixosModules.default = import ./nix/module.nix self;

      checks = forEach (pkgs: {
        package = self.packages.${pkgs.stdenv.hostPlatform.system}.alogeno;
        # Two installations in a NixOS virtual machine.
        module = pkgs.testers.runNixOSTest (import ./nix/test.nix self);
      });

      devShells = forEach (pkgs: {
        default = import ./shell.nix { inherit pkgs; };
      });
    };
}
