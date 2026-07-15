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
      # (~/.claude/skills/corral-claude); its hooks run on node, pulled in below.
      claude.enable = lib.mkOption {
        type = lib.types.bool;
        default = true;
        description = ''
          Symlink the Claude Code adapter into ~/.claude/skills (loads as
          corral-claude@skills-dir) and put node on PATH for its hooks.
        '';
      };
      # The Cursor adapter is a VS Code extension directory loaded from
      # ~/.cursor/extensions; its state-hook runs on node, pulled in below.
      cursor.enable = lib.mkOption {
        type = lib.types.bool;
        default = true;
        description = ''
          Symlink the Cursor adapter into ~/.cursor/extensions
          (corral-cursor VS Code extension) and put node on PATH for its
          state-hook.
        '';
      };
    };
  };

  config = lib.mkIf cfg.enable {
    # The Claude adapter's hooks run as external `node hook.ts` subprocesses
    # (Claude ships no runtime for hook commands), unlike the pi/opencode
    # adapters that the harness itself loads. node (not bun: bun's JSC
    # SIGTRAP-crashes under Landlock sandboxes) runs the .ts directly via its
    # native type-stripping (nodejs >= 22.18 / 24). So node is a hard
    # dependency whenever the adapter is linked.
    # node (not bun: bun's JSC SIGTRAP-crashes under Landlock sandboxes) is a
    # hard dependency for the Claude hooks and the Cursor state-hook, both of
    # which run as external `node` subprocesses the harness does not provide.
    home.packages = [ cfg.package ]
      ++ lib.optional
        (cfg.extensions.claude.enable || cfg.extensions.cursor.enable)
        pkgs.nodejs;

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
        ".pi/agent/extensions/corral-pi.ts".source =
          "${extDir}/corral-pi.ts";
      })
      (lib.mkIf cfg.extensions.opencode.enable {
        ".config/opencode/plugin/corral-opencode.ts".source =
          "${extDir}/corral-opencode.ts";
      })
      (lib.mkIf cfg.extensions.claude.enable {
        ".claude/skills/corral-claude".source = "${extDir}/corral-claude";
      })
      (lib.mkIf cfg.extensions.cursor.enable {
        ".cursor/extensions/corral-cursor".source = "${extDir}/corral-cursor";
      })
    ];
  };
}
