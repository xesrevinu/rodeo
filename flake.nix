{
  description = "rodeo — terminal UI for herdr-core";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    rust-overlay.inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
    }:
    let
      systems = [
        "aarch64-darwin"
        "x86_64-darwin"
        "aarch64-linux"
        "x86_64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
      zigVersion = "0.15.2";
      zigAssets = {
        aarch64-darwin = {
          url = "https://ziglang.org/download/${zigVersion}/zig-aarch64-macos-${zigVersion}.tar.xz";
          hash = "sha256-PMK6s2fhhc37J1AcSzCxsGU8KNn3PfjckUiOZuzl+ms=";
        };
        x86_64-darwin = {
          url = "https://ziglang.org/download/${zigVersion}/zig-x86_64-macos-${zigVersion}.tar.xz";
          hash = "sha256-placeholder-x86_64-macos";
        };
        aarch64-linux = {
          url = "https://ziglang.org/download/${zigVersion}/zig-aarch64-linux-${zigVersion}.tar.xz";
          hash = "sha256-placeholder-aarch64-linux";
        };
        x86_64-linux = {
          url = "https://ziglang.org/download/${zigVersion}/zig-x86_64-linux-${zigVersion}.tar.xz";
          hash = "sha256-placeholder-x86_64-linux";
        };
      };
      zigPkg =
        pkgs: system:
        let
          asset =
            zigAssets.${system}
              or (throw "no zig ${zigVersion} tarball for ${system}");
        in
        pkgs.stdenvNoCC.mkDerivation {
          pname = "zig";
          version = zigVersion;
          src = pkgs.fetchurl asset;
          dontConfigure = true;
          dontBuild = true;
          dontFixup = true;
          installPhase = ''
            runHook preInstall
            mkdir -p $out/bin
            cp -R . $out/
            ln -s $out/zig $out/bin/zig
            runHook postInstall
          '';
        };
      # Zig 0.15.2 cannot parse the macOS 26+/27 SDK. Link against the 15.4
      # CLT SDK still on this machine. Impure on purpose: the SDK lives
      # outside the store. minos 12.0 covers the 2018 Intel MacBook Pro.
      sdkRoot = "/Library/Developer/CommandLineTools/SDKs/MacOSX15.4.sdk";
      macosMin = "12.0";
      xcrun15 =
        pkgs:
        pkgs.writeShellScriptBin "xcrun" ''
          for arg in "$@"; do
            if [ "$arg" = "--show-sdk-path" ]; then
              echo ${sdkRoot}
              exit 0
            fi
            if [ "$arg" = "--show-sdk-version" ]; then
              echo 15.4
              exit 0
            fi
          done
          exec /usr/bin/xcrun "$@"
        '';
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ (import rust-overlay) ];
          };
          inherit (pkgs) lib;
          rust = pkgs.rust-bin.stable."1.99.1".default;
          rustPlatform = pkgs.makeRustPlatform {
            cargo = rust;
            rustc = rust;
          };
          zig = zigPkg pkgs system;
        in
        rec {
          default = rodeo;
          inherit zig;
          rodeo = rustPlatform.buildRustPackage {
            pname = "rodeo";
            version = "0.2.1";
            src = ./.;
            cargoLock.lockFile = ./Cargo.lock;
            nativeBuildInputs = [
              rust
              zig
            ]
            ++ lib.optionals pkgs.stdenv.hostPlatform.isDarwin [ (xcrun15 pkgs) ];
            # libghostty-vt-sys compiles Ghostty with zig 0.15.2, which cannot
            # parse the macOS 26+/27 SDK. Point at the 15.4 CLT SDK still on
            # this machine. Impure on purpose: the SDK lives outside the store.
            env = lib.optionalAttrs pkgs.stdenv.hostPlatform.isDarwin {
              SDKROOT = sdkRoot;
              MACOSX_DEPLOYMENT_TARGET = macosMin;
            };
            doCheck = false;
            meta = {
              description = "Terminal UI for herdr-core";
              homepage = "https://github.com/lukeed/rodeo";
              license = lib.licenses.mit;
              mainProgram = "rodeo";
            };
          };
        }
      );

      devShells = forAllSystems (
        system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ (import rust-overlay) ];
          };
          rust = pkgs.rust-bin.stable."1.99.1".default.override {
            extensions = [
              "rust-src"
              "clippy"
              "rustfmt"
            ];
          };
          zig = zigPkg pkgs system;
        in
        {
          default = pkgs.mkShell {
            packages = [
              rust
              zig
              pkgs.pkg-config
            ]
            ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isDarwin [ (xcrun15 pkgs) ];
            env = pkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isDarwin {
              SDKROOT = sdkRoot;
              MACOSX_DEPLOYMENT_TARGET = macosMin;
            };
          };
        }
      );
    };
}
