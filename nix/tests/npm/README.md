# npm meta-packages for the VM e2e tests

Each directory is a one-dependency npm project pinning a published package
(`pi`, `mock-llm`) the way a user installs it. `nix/tests/pkgs.nix` builds them
with `buildNpmPackage`.

Regenerating a lockfile (after bumping the version in package.json):

```sh
cd nix/tests/npm/<name>
npm install --package-lock-only --ignore-scripts
```

pi only: `@earendil-works/pi-coding-agent` ships an npm-shrinkwrap, so npm
records its three bundled `@earendil-works/*` nested deps without `integrity`
fields, which `prefetch-npm-deps` rejects. Patch them from the registry:

```sh
python3 - <<'EOF'
import json, urllib.request
p = "package-lock.json"
d = json.load(open(p))
for k, v in d["packages"].items():
    if k and "integrity" not in v and "resolved" in v:
        name = k.split("node_modules/")[-1]
        meta = json.load(urllib.request.urlopen(
            f"https://registry.npmjs.org/{name}/{v['version']}"))
        v["integrity"] = meta["dist"]["integrity"]
json.dump(d, open(p, "w"), indent=2)
EOF
```

Then refresh the hash in `nix/tests/pkgs.nix`:

```sh
nix run nixpkgs#prefetch-npm-deps -- ./package-lock.json
```
