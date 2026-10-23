# The Windlass executable, built and tested from this repository.
{ lib, rustPlatform }:

rustPlatform.buildRustPackage {
  pname = "windlass";
  version = "0.1.0";

  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../src
      ../tests
    ];
  };

  cargoLock.lockFile = ../Cargo.lock;

  meta = {
    description = "A cohesive MAM acquisition and library service";
    mainProgram = "windlass";
    platforms = lib.platforms.linux;
  };
}
