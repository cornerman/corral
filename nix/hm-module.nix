# Home Manager module for corral.
#
# Enable with `programs.corral.enable = true;` to install the three binaries
# (corral, corral-gui, corrald), run corrald as a user service, and symlink the
# pi/opencode/claude adapters into each harness's plugin directory. The Cursor
# adapter is a VS Code extension, so it is installed from a .vsix via
# `programs.corral.cursor.package` (opt-in) instead of symlinked. When
# `programs.git.enable` is also on, it adds `.corral/` to the managed global
# gitignore (every project dir an agent touches gains one); otherwise it emits
# a `warnings` entry rather than silently doing nothing, since writing to some
# other core.excludesfile mechanism (a static dotfile, manual git config) would
# be a second, ineffective writer.
#
# Sandboxed agents (nono, bwrap, or similar) need exactly one write exception
# in their profile: $HOME/.corral/input. This module does not manage that,
# since it does not know which sandbox tool (if any) wraps each harness --
# that decision, and the profile syntax, belong to the sandboxing setup, not
# to corral's own module (see SECURITY.md, Deployment Preconditions).
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
  # The Cursor adapter is a VS Code extension, installed as a .vsix (a separate
  # flake output) rather than symlinked: Cursor only loads extensions it has
  # registered in extensions.json, which a bare symlink never does.
  cursorVsix = "${self.packages.${pkgs.stdenv.hostPlatform.system}.corral-cursor-vsix}/corral-cursor.vsix";
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

    gitignore.enable = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = ''
        Let corral manage the ".corral/" entry in your global gitignore (via
        programs.git.ignores, only takes effect when programs.git.enable is
        also on -- else a warning fires instead). Set to false once you've
        added ".corral/" to your own gitignore yourself, or if you don't track
        this at all, to silence both the write attempt and the warning -- a
        `warnings` entry has no "seen once" memory and would otherwise repeat
        on every rebuild.
      '';
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
    };

    # The Cursor adapter is a VS Code extension: it must be *installed* (so Cursor
    # registers it in extensions.json), not symlinked. Set this to your Cursor
    # package (e.g. pkgs.code-cursor) to install the corral-cursor .vsix via
    # `cursor --install-extension` on activation; null (default) leaves Cursor
    # untouched. Kept off the `extensions` submodule because it needs the Cursor
    # binary, not just a symlink.
    cursor.package = lib.mkOption {
      type = lib.types.nullOr lib.types.package;
      default = null;
      example = lib.literalExpression "pkgs.code-cursor";
      description = ''
        Cursor package used to install the corral-cursor VS Code extension
        (its .vsix) on activation. null disables Cursor integration.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    # Add .corral/ to the user's global gitignore -- but only if home-manager
    # actually owns core.excludesfile (programs.git.enable). If git is managed
    # some other way (a static dotfile, manual `git config`), writing here would
    # be a silent second writer with no effect on the file core.excludesfile
    # really points at. Fail loud instead (a warning), rather than silently
    # doing nothing: the user then adds ".corral/" to their own gitignore by
    # hand and sets gitignore.enable = false to stop the (otherwise permanent,
    # since `warnings` has no "seen once" memory) nag.
    programs.git.ignores = lib.mkIf (cfg.gitignore.enable && config.programs.git.enable) [ ".corral/" ];

    warnings = lib.optional (cfg.gitignore.enable && !config.programs.git.enable) ''
      corral: programs.git.enable is false, so corral cannot add .corral/ to
      your global gitignore automatically. Add it to your own core.excludesfile
      by hand, then set programs.corral.gitignore.enable = false to silence
      this -- every project directory an agent touches gains a .corral/.
    '';

    # The Claude adapter's hooks run as external `node hook.ts` subprocesses
    # (Claude ships no runtime for hook commands), unlike the pi/opencode
    # adapters that the harness itself loads. node (not bun: bun's JSC
    # SIGTRAP-crashes under Landlock sandboxes) runs the .ts directly via its
    # native type-stripping (nodejs >= 22.18 / 24). So node is a hard
    # dependency whenever the adapter is linked. The Cursor state-hook likewise
    # runs as an external `node` subprocess, so node is needed when Cursor is on.
    home.packages = [ cfg.package ]
      ++ lib.optional
        (cfg.extensions.claude.enable || cfg.cursor.package != null)
        pkgs.nodejs;

    # Install the Cursor extension idempotently. Guarded on the .vsix store path
    # (a marker file) so it runs only when the vsix changes, not every rebuild.
    # Uses Cursor's own registration (extensions.json), coexisting with
    # hand-installed extensions. Never fails activation: a broken editor must not
    # block a rebuild. Runs after writeBoundary so $HOME writes are settled.
    #
    # `cursor --install-extension` exits 0 even when it actually fails (e.g.
    # EACCES on the extensions dir), so its exit code is not trustworthy. Verify
    # the ground truth instead: the marker is written only once corral-cursor
    # appears in extensions.json. A silent failure leaves no marker, so the next
    # activation retries rather than latching a false success (fail loud, retry).
    home.activation.corralCursorExtension = lib.mkIf (cfg.cursor.package != null)
      (lib.hm.dag.entryAfter [ "writeBoundary" ] ''
        marker="$HOME/.cursor/.corral-cursor-vsix"
        want="${cursorVsix}"
        registry="$HOME/.cursor/extensions/extensions.json"
        if [ "$(cat "$marker" 2>/dev/null)" != "$want" ]; then
          mkdir -p "$HOME/.cursor"
          ${cfg.cursor.package}/bin/cursor --install-extension "$want" --force || true
          if grep -q corral-cursor "$registry" 2>/dev/null; then
            printf '%s' "$want" > "$marker"
          else
            echo "corral: cursor did not register corral-cursor (not in $registry); will retry next activation" >&2
          fi
        fi
      '');

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
    ];
  };
}
