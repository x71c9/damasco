{ pkgs ? import <nixpkgs> {} }:
pkgs.mkShell {
  packages = [
    pkgs.cargo
    pkgs.rustc
    pkgs.rustfmt
    pkgs.clippy
  ];
  shellHook = ''
    echo "cargo: $(cargo -V) | rustc: $(rustc -V)"
  '';
}
