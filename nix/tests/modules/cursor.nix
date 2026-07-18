# e2e-cursor node overlay: install code-cursor (unfree) and wire the corral
# Cursor extension (.vsix) through the HM module. Cursor's Composer is
# login-gated, so this scenario is partial by design (announce/focus/state
# hooks + the pure-core unit tests); no agent turns.
# allowUnfree comes from the host pkgs (pkgsUnfree in default.nix); the test
# framework pins node pkgs read-only, so it must not be set again here.
{ pkgs, ... }:
{
  environment.systemPackages = [ pkgs.code-cursor ];

  # Installs the corral-cursor .vsix via `cursor --install-extension` on
  # activation (best-effort; never blocks the build).
  home-manager.users.alice.programs.corral.cursor.package = pkgs.code-cursor;
}
