# Vendored pi lockfile for the VM e2e tests

`pi/npm-shrinkwrap.json` is a copy of the `npm-shrinkwrap.json` bundled inside
the `@earendil-works/pi-coding-agent` release tarball, with one change: pi
references its own sibling packages (`@earendil-works/pi-tui`, `pi-ai`,
`pi-agent-core`) without `integrity` fields, which breaks offline `npm ci`, so
those three integrities are patched in from the registry. `nix/tests/pkgs.nix`
swaps this file into the source before fetching deps.

Regenerating (after bumping the version in `pkgs.nix`):

```sh
V=0.80.10
cd /tmp
curl -sL "https://registry.npmjs.org/@earendil-works/pi-coding-agent/-/pi-coding-agent-$V.tgz" -o pi.tgz
tar xzf pi.tgz
python3 - <<'EOF'
import json, urllib.request
d = json.load(open("package/npm-shrinkwrap.json"))
for k, v in d["packages"].items():
    if k and "integrity" not in v and v.get("resolved", "").startswith("http"):
        name = k.split("node_modules/")[-1]
        meta = json.load(urllib.request.urlopen(
            f"https://registry.npmjs.org/{name}/{v['version']}"))
        v["integrity"] = meta["dist"]["integrity"]
json.dump(d, open("out.json", "w"), indent=2)
EOF
cp out.json <repo>/nix/tests/npm/pi/npm-shrinkwrap.json
nix run nixpkgs#prefetch-npm-deps -- out.json   # -> npmDepsHash in pkgs.nix
```

The tarball hash in `pkgs.nix` comes from
`nix-prefetch-url <tarball-url> | xargs nix hash to-sri --type sha256`.
