{
  description = "damasco: self-hosted website builder that works with an existing AI subscription";

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
        damasco = pkgs.callPackage ./nix/package.nix { };
        default = damasco;
      });

      overlays.default = final: _prev: {
        damasco = final.callPackage ./nix/package.nix { };
      };

      # services.damasco: the default installation and the named ones.
      nixosModules.default = import ./nix/module.nix self;

      checks = forEach (pkgs: {
        package = self.packages.${pkgs.stdenv.hostPlatform.system}.damasco;
        # Two installations in a NixOS virtual machine.
        module = pkgs.testers.runNixOSTest (import ./nix/test.nix self);
      });

      devShells = forEach (pkgs: {
        default = import ./shell.nix { inherit pkgs; };
      });
    };
}
