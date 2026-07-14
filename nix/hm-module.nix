# Home Manager module for corral.
#
# Enable with `programs.corral.enable = true;` to install the three binaries
# (corral, corral-gui, corrald), run corrald as a user service, and symlink the
# harness adapters into each harness's plugin directory.
#
# `self` is the corral flake, so the module resolves corral's own package for
# the current system without the importing config having to name it.
self:
{ config, lib, pkgs, ... }:

let
  cfg = config.programs.corral;
  # Extension files ship inside the package under share/corral/extensions, so
  # the module references one artifact rather than the flake source tree.
  extDir = "${cfg.package}/share/corral/extensions";
in
{
  options.programs.corral = {
    enable = lib.mkEnableOption "corral, the attention board for local ACP agents";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      defaultText = lib.literalExpression "corral.packages.\${system}.default";
      description = "The corral package providing corral, corral-gui and corrald.";
    };

    daemon.enable = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = ''
        Run corrald (the inter-agent messaging daemon) as a systemd user
        service. Bound to graphical-session.target because its approval gate
        surfaces on a tray and desktop notifications.
      '';
    };

    extensions = {
      pi.enable = lib.mkOption {
        type = lib.types.bool;
        default = true;
        description = "Symlink the pi adapter into ~/.pi/agent/extensions.";
      };
      opencode.enable = lib.mkOption {
        type = lib.types.bool;
        default = true;
        description = "Symlink the opencode adapter into ~/.config/opencode/plugin.";
      };
      # The Claude adapter is a plugin directory loaded as a skills-dir plugin
      # (~/.claude/skills/corral-claude); its hooks run on bun, pulled in below.
      claude.enable = lib.mkOption {
        type = lib.types.bool;
        default = true;
        description = ''
          Symlink the Claude Code adapter into ~/.claude/skills (loads as
          corral-claude@skills-dir) and put bun on PATH for its hooks.
        '';
      };
    };
  };

  config = lib.mkIf cfg.enable {
    # The Claude adapter's hooks run as external `bun hook.ts` subprocesses
    # (Claude ships no runtime for hook commands), unlike the pi/opencode
    # adapters that the harness itself loads. So bun is a hard dependency
    # whenever the adapter is linked.
    home.packages = [ cfg.package ]
      ++ lib.optional cfg.extensions.claude.enable pkgs.bun;

    # corrald is a singleton; the user service is its keep-alive. It reads the
    # filesystem registry and owns the control socket, so it needs no ordering
    # beyond the graphical session that hosts its tray.
    systemd.user.services.corrald = lib.mkIf cfg.daemon.enable {
      Unit = {
        Description = "corral inter-agent messaging daemon";
        PartOf = [ "graphical-session.target" ];
        After = [ "graphical-session.target" ];
      };
      Service = {
        ExecStart = "${cfg.package}/bin/corrald";
        Restart = "on-failure";
        RestartSec = 2;
      };
      Install.WantedBy = [ "graphical-session.target" ];
    };

    home.file = lib.mkMerge [
      (lib.mkIf cfg.extensions.pi.enable {
        ".pi/agent/extensions/corral-announce.ts".source =
          "${extDir}/corral-announce.ts";
      })
      (lib.mkIf cfg.extensions.opencode.enable {
        ".config/opencode/plugin/corral-opencode.ts".source =
          "${extDir}/corral-opencode.ts";
      })
      (lib.mkIf cfg.extensions.claude.enable {
        ".claude/skills/corral-claude".source = "${extDir}/corral-claude";
      })
    ];
  };
}
