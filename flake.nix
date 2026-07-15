{
  description = "corral - discover and manage locally running ACP coding agents";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, rust-overlay }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" "aarch64-darwin" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system:
        f (import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        }));
      # Pin the Rust toolchain via rust-overlay; rust-toolchain.toml is the
      # single source of truth for the version.
      toolchainFor = pkgs: pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
      # The GUI shell (corral-gui, iced) links these graphics libraries.
      # winit needs the Wayland + X11 client libs and libxkbcommon; iced's wgpu
      # renderer needs the Vulkan loader (with a libGL fallback). libxcb is a
      # link-time dep of the X11 path. On mainstream distros these already sit
      # in standard paths, so a built binary just runs; NixOS needs them named
      # explicitly (build + runtime).
      guiLibsFor = pkgs: with pkgs; [
        vulkan-loader
        libGL
        libxkbcommon
        wayland
        libx11
        libxcursor
        libxi
        libxrandr
        libxcb
      ];
    in
    {
      packages = forAllSystems (pkgs: {
        default = pkgs.rustPlatform.buildRustPackage {
          pname = "corral";
          version = "0.1.0";
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;
          nativeBuildInputs = [ pkgs.pkg-config pkgs.makeWrapper ];
          buildInputs = guiLibsFor pkgs;
          # egui/winit dlopen libGL and the display libs at runtime; on NixOS
          # they are not in a standard path, so put them on the GUI binary's
          # library path. Harmless no-op on the TUI/daemon binaries.
          postInstall = ''
            wrapProgram "$out/bin/corral-gui" \
              --prefix LD_LIBRARY_PATH : "${pkgs.lib.makeLibraryPath (guiLibsFor pkgs)}"
            # Ship the harness adapters beside the binaries so the home-manager
            # module symlinks from one artifact, not the flake source tree.
            mkdir -p "$out/share/corral/extensions"
            cp extensions/corral-pi.ts extensions/corral-opencode.ts \
              "$out/share/corral/extensions/"
            # The Claude and Cursor adapters are directories (a plugin tree and
            # a VS Code extension), so ship the whole tree, not a single file.
            cp -r extensions/corral-claude extensions/corral-cursor \
              "$out/share/corral/extensions/"
          '';
        };
      });

      # Home Manager module: `programs.corral.enable = true;` installs the
      # binaries, runs corrald as a user service, and links the adapters.
      homeManagerModules = rec {
        corral = import ./nix/hm-module.nix self;
        default = corral;
      };

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = [
            (toolchainFor pkgs)
            pkgs.just
            pkgs.cargo-watch
            pkgs.pkg-config
          ];
          buildInputs = guiLibsFor pkgs;
          # So `cargo run -p corral-gui` finds libGL / wayland / X11 at runtime.
          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath (guiLibsFor pkgs);
        };
      });

      checks = forAllSystems (pkgs: {
        build = self.packages.${pkgs.system}.default;
      });
    };
}
