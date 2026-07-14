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
      # The Claude Code adapter is not yet on main; the option exists so a
      # future release can wire it without a breaking rename.
      claude.enable = lib.mkOption {
        type = lib.types.bool;
        default = false;
        description = "Install the Claude Code adapter (not yet available).";
      };
    };
  };

  config = lib.mkIf cfg.enable {
    home.packages = [ cfg.package ];

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
    ];

    assertions = [{
      assertion = !cfg.extensions.claude.enable;
      message = "programs.corral.extensions.claude is not yet available on this release.";
    }];
  };
}
