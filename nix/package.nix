# The alogeno binary with the panel embedded: the same build as
# scripts/package, reproducible. The panel is built first and copied where
# rust-embed looks for it (panel/dist).
{
  lib,
  rustPlatform,
  buildNpmPackage,
  nodejs_26,
  pkg-config,
  sqlite,
}:

let
  version = (lib.importTOML ../Cargo.toml).workspace.package.version;

  panel = buildNpmPackage {
    pname = "alogeno-panel";
    inherit version;
    src = ../panel;
    nodejs = nodejs_26;
    # The panel keeps version 0.0.0 in package.json, so that a release
    # leaves this hash alone: the version of alogeno is the one of Cargo.toml.
    npmDepsHash = "sha256-SvHcFl0tOfVjU5z3BINJ0Cdp0g5AI9eZ0z3vXKvJ8LE=";
    installPhase = ''
      runHook preInstall
      cp -r dist $out
      runHook postInstall
    '';
  };
in
rustPlatform.buildRustPackage {
  pname = "alogeno";
  inherit version;

  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../crates
      ../site-template
    ];
  };
  cargoLock.lockFile = ../Cargo.lock;

  # The build links the SQLite of nixpkgs instead of compiling the bundled one.
  nativeBuildInputs = [ pkg-config ];
  buildInputs = [ sqlite ];

  preBuild = ''
    mkdir -p panel
    cp -r ${panel} panel/dist
    chmod -R u+w panel/dist
  '';

  # The tests start bubblewrap sandboxes and git, which the build sandbox
  # does not allow; they run in the CI of the repository.
  doCheck = false;

  passthru = { inherit panel; };

  meta = {
    description = "Self-hosted website builder that works with an existing AI subscription";
    homepage = "https://github.com/x71c9/alogeno";
    license = lib.licenses.agpl3Only;
    mainProgram = "alogeno";
    platforms = lib.platforms.linux;
  };
}
