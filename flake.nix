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

        # The Cursor adapter packaged as an installable VS Code extension (.vsix).
        # Cursor does not load a bare folder dropped in ~/.cursor/extensions; it
        # only loads what is registered in extensions.json. So the home-manager
        # module installs this .vsix with `cursor --install-extension`, which uses
        # Cursor's own registration. A .vsix is a zip in the standard VSIX 2.0.0
        # layout (extension.vsixmanifest + [Content_Types].xml at the root, files
        # under extension/); built by hand with zip so there is no vsce/node build
        # dependency, matching the adapter's no-build ethos.
        corral-cursor-vsix = pkgs.stdenv.mkDerivation {
          pname = "corral-cursor-vsix";
          version = "0.1.0";
          src = ./extensions/corral-cursor;
          nativeBuildInputs = [ pkgs.zip ];
          dontConfigure = true;
          # The two VSIX metadata files are generated via writeText (not shell
          # heredocs, which nix's indentation stripping would break) and copied
          # in beside the extension/ payload before zipping.
          buildPhase = ''
            mkdir -p pkg/extension
            cp -r "$src"/. pkg/extension/
            # Files copied from the store are read-only (444). Cursor rewrites
            # the extracted package.json to inject install metadata, so the
            # zipped files MUST be writable or `--install-extension` fails with
            # EACCES on that rewrite (and exits 0, hiding it).
            chmod -R u+w pkg
            cp ${pkgs.writeText "extension.vsixmanifest" ''
              <?xml version="1.0" encoding="utf-8"?>
              <PackageManifest Version="2.0.0" xmlns="http://schemas.microsoft.com/developer/vsx-schema/2011" xmlns:d="http://schemas.microsoft.com/developer/vsx-schema-design/2011">
                <Metadata>
                  <Identity Language="en-US" Id="corral-cursor" Version="0.1.0" Publisher="corral"/>
                  <DisplayName>corral for Cursor</DisplayName>
                  <Description>Announce this Cursor window to a corral board.</Description>
                </Metadata>
                <Installation>
                  <InstallationTarget Id="Microsoft.VisualStudio.Code"/>
                </Installation>
                <Dependencies/>
                <Assets>
                  <Asset Type="Microsoft.VisualStudio.Code.Manifest" Path="extension/package.json" Addressable="true"/>
                </Assets>
              </PackageManifest>
            ''} pkg/extension.vsixmanifest
            cp ${pkgs.writeText "content-types.xml" ''
              <?xml version="1.0" encoding="utf-8"?>
              <Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
                <Default Extension="json" ContentType="application/json"/>
                <Default Extension="js" ContentType="application/javascript"/>
                <Default Extension="md" ContentType="text/markdown"/>
                <Default Extension="vsixmanifest" ContentType="text/xml"/>
              </Types>
            ''} 'pkg/[Content_Types].xml'
          '';
          installPhase = ''
            mkdir -p "$out"
            ( cd pkg && zip -r -X "$out/corral-cursor.vsix" \
                extension.vsixmanifest '[Content_Types].xml' extension >/dev/null )
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
