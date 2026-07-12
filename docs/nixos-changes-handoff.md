# Handoff: Changes Wanted in the nixos Repo (~/home/cornerman/nixos)

Context: the corral attention board now lives in `~/projects/corral` on branch
`corral-board`. The pi sandbox must let sandboxed sessions edit the board crate,
and (temporarily) the nixos config. These edits are in the nixos repo, which the
corral-workdir session cannot write.

## 1. Sandbox Profiles (Required)

In BOTH files:
- `home/nono/profiles/pi-minimal.json`
- `home/nono/profiles/pi-maximal.json`

Ensure the `filesystem.allow` array (readwrite) contains these two entries,
right after `"$WORKDIR"`:

```json
      "$WORKDIR",
      "$HOME/nixos",
      "$HOME/projects/corral/crates/board",
      "$HOME/.corral",
      "$HOME/.pi",
```

- `$HOME/nixos` — may already be present (added earlier this session, possibly
  uncommitted); keep it. Temporary, so sandboxed sessions can edit the config.
- `$HOME/projects/corral/crates/board` — new. Lets sandboxed sessions develop
  the board crate while the rest of `~/projects/corral` stays read-only (it is
  in the `read` list already; leave that).
- `$HOME/.corral` — ALREADY APPLIED. This is the ACP discovery dir: the
  corral-announce extension binds `sockets/pi-<pid>.sock` here and corral scans
  it (it replaced `$XDG_RUNTIME_DIR/acp`, which the sandbox does not expose).
  New pi sessions pick it up; sessions started before it was allowed still bind
  the old path.

Then activate (needs sudo, so the user runs it):

```bash
sudo nixos-rebuild switch --flake /home/cornerman/nixos#wolf
```

Verify the JSON stays valid (both profiles parse) and the build succeeds with
`nixos-rebuild build` before switch.

## 2. After corral-board Merges to main (Later, Not Now)

The nixos flake input `corral` (git+file at `~/projects/corral`) tracks committed
files on the default branch. Once `corral-board` is merged into `main` there:

```bash
cd /home/cornerman/nixos
nix flake update corral
sudo nixos-rebuild switch --flake .#wolf
```

This picks up the new `corral` binary (the board; package name is unchanged) and
the updated `corral-announce.ts`. No edits to `home/ai-agents/corral.nix` are
needed: it still builds `inputs.corral.packages.default` (binary still named
`corral`), and the extension is an out-of-store symlink to the live checkout, so
it already reflects the branch working tree.

## Do NOT

- Do not remove `$HOME/projects/corral` from the `read` list.
- Do not change `home/ai-agents/corral.nix` (the pi-acp revert and the symlink
  are already committed and correct).
