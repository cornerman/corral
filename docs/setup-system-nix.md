# Task: Install corral System-Wide via Nix

You are working on the user's Nix configuration (likely `~/nixos`, flake-based,
with home-manager). Goal: make the `agentwrap` and `corral` binaries available
on the user's PATH, declaratively.

## What corral Is

Local flake at `/home/cornerman/projects/corral` (Rust workspace, two binaries
in one package, default package output `corral`):

- `agentwrap` — wraps an ACP-mode coding agent, exposes its stdio on
  `$XDG_RUNTIME_DIR/acp/<label>-<pid>.sock`
- `corral` — live table of all discovered agent sockets

Verify it builds before wiring anything: `nix build /home/cornerman/projects/corral`

## Steps

1. Add the flake input to the system/home flake:

   ```nix
   inputs.corral.url = "git+file:///home/cornerman/projects/corral";
   ```

   Note: git+file only sees *committed* files. If the build misses files,
   check `git -C /home/cornerman/projects/corral status`.

2. Install the package for user `cornerman` (home-manager preferred):

   ```nix
   home.packages = [ inputs.corral.packages.${pkgs.system}.default ];
   ```

   Pass `inputs` through `extraSpecialArgs`/`specialArgs` if the module does
   not already receive it — check how other local flake inputs are wired in
   this config and follow the existing pattern.

3. Add shell aliases (home-manager `programs.<shell>.shellAliases`), matching
   whatever ACP agent binaries exist on the system — check PATH first, only
   alias what is installed:

   ```nix
   shellAliases = {
     claude-w = "agentwrap --name claude -- claude-agent-acp";
     codex-w = "agentwrap --name codex -- codex-acp";
   };
   ```

4. Rebuild (use the repo's usual command, e.g. `nh os switch` or
   `nixos-rebuild switch --flake`), then verify:

   ```bash
   which agentwrap corral
   corral --once   # prints table header, "(no agent sockets found)" is fine
   ```

## Constraints

- Do not install anything imperatively (`nix profile install` etc.) — the
  user wants it declarative only.
- Follow existing conventions in the nixos repo; do not restructure it.
- Updating corral later = `nix flake update corral` in the config repo
  (the input is pinned by hash at lock time).
