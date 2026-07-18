# VM e2e smoke tests. Four flake checks, one per harness, all on the shared
# base VM (base.nix). See docs/superpowers/specs/2026-07-18-vm-e2e-smoke-test-design.md.
#
# claude-code and code-cursor are unfree, so those two checks are built on a
# nixpkgs instance with allowUnfree; pi/opencode stay on the free default. Each
# testScript is the shared prelude.py plus the scenario file.
{ pkgs, self, home-manager }:

let
  # Unfree nixpkgs for the claude/cursor scenarios (same channel as `pkgs`).
  pkgsUnfree = import pkgs.path {
    inherit (pkgs.stdenv.hostPlatform) system;
    config.allowUnfree = true;
  };

  prelude = builtins.readFile ./prelude.py;

  mkTest = hostPkgs: { name, modules ? [ ], scenario }:
    let
      testPkgs = import ./pkgs.nix { pkgs = hostPkgs; };
      base = import ./base.nix { inherit self home-manager testPkgs; };
    in
    hostPkgs.testers.runNixOSTest {
      inherit name;
      enableOCR = true;
      nodes.machine = { ... }: { imports = [ base ] ++ modules; };
      testScript = prelude + "\n\n# --- scenario ---\n" + builtins.readFile scenario;
    };
in
{
  e2e-pi = mkTest pkgs {
    name = "corral-e2e-pi";
    scenario = ./scenarios/pi.py;
  };
  e2e-opencode = mkTest pkgs {
    name = "corral-e2e-opencode";
    scenario = ./scenarios/opencode.py;
  };
  e2e-claude = mkTest pkgsUnfree {
    name = "corral-e2e-claude";
    modules = [ ./modules/claude.nix ];
    scenario = ./scenarios/claude.py;
  };
  e2e-cursor = mkTest pkgsUnfree {
    name = "corral-e2e-cursor";
    modules = [ ./modules/cursor.nix ];
    scenario = ./scenarios/cursor.py;
  };
}
