{
  description = "lki-top: a btop-inspired system monitor built on gpui";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
        cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);

        # gpui links xkbcommon and xcb at build time. Wayland, Vulkan
        # (blade renderer) and fontconfig (font-kit) are dlopened at
        # runtime, so the linker never sees them and they have to be put
        # on the rpath by hand (package) or on LD_LIBRARY_PATH (shell).
        gpuiLibs = with pkgs; [
          libxkbcommon
          libxcb
          libx11
          wayland
          vulkan-loader
          fontconfig
          freetype
        ];

        lki-top = pkgs.rustPlatform.buildRustPackage {
          pname = "lki-top";
          version = cargoToml.package.version;
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;
          nativeBuildInputs = with pkgs; [ pkg-config ];
          buildInputs = gpuiLibs;
          postFixup = ''
            patchelf --add-rpath ${pkgs.lib.makeLibraryPath gpuiLibs} $out/bin/lki-top
          '';
          meta = {
            description = cargoToml.package.description;
            license = pkgs.lib.licenses.mit;
            mainProgram = "lki-top";
          };
        };
      in {
        packages.default = lki-top;

        devShells.default = pkgs.mkShell {
          nativeBuildInputs = with pkgs; [
            rustc
            cargo
            rustfmt
            clippy
            rust-analyzer
            pkg-config
          ];
          buildInputs = gpuiLibs;
          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath gpuiLibs;
        };
      });
}
