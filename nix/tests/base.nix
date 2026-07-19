# Base VM for the corral e2e smoke tests: sway (headless, software rendering) +
# kitty, autologin user `alice`, corral installed through its own home-manager
# module (so the test validates the shipped install path), the stub LLM as a
# system service, and nono-wrapped harness binaries so every agent launch is
# confined. Scenario modules add the harness they exercise.
{ self, home-manager, testPkgs }:
{ pkgs, lib, config, ... }:

let
  user = "alice";
  home = "/home/${user}";
  uid = 1000;

  # The stub LLM (nix/tests/stub-llm.py) as a runnable script.
  stubLlm = pkgs.writeShellScriptBin "corral-stub-llm" ''
    exec ${pkgs.python3}/bin/python3 ${./stub-llm.py} "$@"
  '';

  # NOTE (2026-07-19): agents run UNCONFINED for the main loop. Getting a full
  # agent (pi's whole node closure) to run under nono needs per-harness path
  # discovery (`nono learn`) that is a separate effort; the security premise is
  # still proven by the sandbox-negative checks in scenarios/pi.py, which run
  # `nono run` directly on cat/sh and assert cross-workdir reads and
  # state/registry writes are DENIED. Confining the whole agent is a tracked
  # follow-up (TODO.md). nono stays on PATH for those direct checks.
in
{
  imports = [ home-manager.nixosModules.home-manager ];

  # --- VM sizing: agents + a browser-class editor need room --------------
  virtualisation.memorySize = 4096;
  virtualisation.diskSize = 8192;
  virtualisation.cores = 4;

  # --- Autologin to a tty that launches sway -----------------------------
  services.getty.autologinUser = user;
  users.users.${user} = {
    isNormalUser = true;
    inherit uid home;
    extraGroups = [ "wheel" "video" ];
    password = "";
  };
  security.sudo.wheelNeedsPassword = false;

  programs.sway.enable = true;
  # Software rendering: no GPU in the test VM. wlroots must be told to accept
  # the pixman (CPU) renderer. lavapipe/llvmpipe cover the GUI's wgpu stack.
  environment.sessionVariables = {
    WLR_RENDERER = "pixman";
    WLR_RENDERER_ALLOW_SOFTWARE = "1";
    WLR_NO_HARDWARE_CURSORS = "1";
    LIBGL_ALWAYS_SOFTWARE = "1";
    PI_TELEMETRY = "0";
  };
  # Launch sway on tty1 login; touch a marker the test waits on.
  programs.bash.loginShellInit = ''
    if [ "$(tty)" = /dev/tty1 ]; then
      export XDG_RUNTIME_DIR=/run/user/${toString uid}
      exec sway
    fi
  '';

  fonts.packages = [ pkgs.dejavu_fonts ];

  environment.systemPackages = [
    pkgs.kitty
    pkgs.python3
    pkgs.jq
    pkgs.nono
    pkgs.mesa # llvmpipe/lavapipe for the GUI under software GL
    stubLlm
    testPkgs.pi
    pkgs.opencode
    self.packages.${pkgs.stdenv.hostPlatform.system}.default # corral, corral-gui, corrald
  ];

  # The vendored nono profile every agent runs under, and the ACP helper the
  # scenarios call.
  environment.etc."corral/agent.jsonc".source = ./profiles/agent.jsonc;
  environment.etc."corral/acp.py".source = ./acp.py;

  # nono refuses to run unless its session store is 0700; the default umask
  # would create it group/world-readable. Pre-create it tight.
  systemd.tmpfiles.rules = [
    "d ${home}/.nono 0700 ${user} users -"
    "d ${home}/.nono/sessions 0700 ${user} users -"
  ];

  # Stub LLM as a system service on 127.0.0.1:6556 (OpenAI + Anthropic).
  systemd.services.corral-stub-llm = {
    description = "corral e2e stub LLM";
    wantedBy = [ "multi-user.target" ];
    serviceConfig = {
      ExecStart = "${stubLlm}/bin/corral-stub-llm 6556";
      Restart = "on-failure";
    };
  };

  # --- corral via its real home-manager module ---------------------------
  home-manager.useGlobalPkgs = true;
  home-manager.users.${user} = { ... }: {
    home.stateVersion = "24.11";
    imports = [ self.homeManagerModules.corral ];

    programs.corral.enable = true;
    # The pi and opencode adapters symlink from the module; the Claude scenario
    # opts in via its own module (mkDefault so it can override).
    programs.corral.extensions.claude.enable = lib.mkDefault false;

    # corrald's unit targets graphical-session.target, which a tty-launched
    # sway does not activate. Retarget it to default.target for the test only.
    systemd.user.services.corrald = {
      Unit = lib.mkForce { Description = "corral inter-agent messaging daemon"; };
      Install = lib.mkForce { WantedBy = [ "default.target" ]; };
    };

    # Pre-seed pi: point at the stub model, trust every project so first start
    # is prompt-free, and load the two test extensions.
    home.file.".pi/agent/settings.json".text = builtins.toJSON {
      defaultModel = "smoke";
      defaultProjectTrust = "always";
      enableInstallTelemetry = false;
    };
    home.file.".pi/agent/extensions/stub-provider.ts".source = ./pi-ext/stub-provider.ts;
    home.file.".pi/agent/extensions/question.ts".source = ./pi-ext/question.ts;

    # Pre-seed the harness-registration store so corrald treats every adapter
    # kind as already registered. corrald normally holds a new kind pending an
    # operator tray approval; the VM has no tray, so we seed the exact launch
    # sets each adapter writes (see extensions/*). Without this, records stay
    # quarantined and never reach the vetted state/registry the boards read.
    home.file.".corral/state/approved-commands.json".text = builtins.toJSON {
      pi = { spawn = [ "pi" ]; resume = [ "pi" "--session" "{sessionId}" ]; };
      opencode = { spawn = [ "opencode" ]; resume = [ "opencode" "--session" "{sessionId}" ]; };
      claude = { spawn = [ "claude" ]; resume = [ "claude" "--resume" "{sessionId}" ]; };
      cursor = { spawn = [ "cursor" "{cwd}" ]; resume = [ "cursor" "{cwd}" ]; gui = true; };
    };

    # Minimal sway config: no bar, mod4, a marker exec the test waits on.
    home.file.".config/sway/config".text = ''
      set $mod Mod4
      default_border none
      exec "touch /tmp/sway-ready"
    '';
  };

  system.stateVersion = "24.11";
}
