{ pkgs ? import <nixpkgs> {} }:
pkgs.mkShell {
  packages = [
    pkgs.cargo
    pkgs.rustc
    pkgs.rustfmt
    pkgs.clippy
    pkgs.nodejs_26
    pkgs.watchexec
  ];
  shellHook = ''
    echo "cargo: $(cargo -V) | rustc: $(rustc -V) | node: $(node -v)"
  '';
}
