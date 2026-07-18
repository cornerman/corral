# e2e-claude node overlay: install claude-code (unfree), enable the Claude
# adapter through the corral HM module, and point Claude at the stub's Anthropic
# endpoint. Node is pulled in by the adapter (its hooks run on node).
# allowUnfree comes from the host pkgs (pkgsUnfree in default.nix); the test
# framework pins node pkgs read-only, so it must not be set again here.
{ pkgs, ... }:
{
  environment.systemPackages = [ pkgs.claude-code ];

  # Claude Code reads these; the stub serves /v1/messages (Anthropic) on 6556.
  environment.sessionVariables = {
    ANTHROPIC_BASE_URL = "http://127.0.0.1:6556";
    ANTHROPIC_API_KEY = "stub-key";
  };

  home-manager.users.alice.programs.corral.extensions.claude.enable = true;
}
