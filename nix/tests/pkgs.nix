# Test-only packages for the VM e2e smoke tests.
#
# pi is a published npm package without a nixpkgs derivation. It is built as an
# npm *meta-package*: a local package.json depending on the published package
# plus a committed package-lock.json, built with buildNpmPackage. This mirrors
# the common `npm install -g pi` setup a real user runs, without coupling the
# repo to any private derivation. (The stub LLM is plain stdlib Python, not an
# npm package -- see nix/tests/stub-llm.py.)
#
# Updating a pin: bump the version in nix/tests/npm/<name>/package.json, run
# `npm install --package-lock-only --ignore-scripts` there, re-run
# `nix run nixpkgs#prefetch-npm-deps -- ./package-lock.json`, and paste the new
# npmDepsHash here. pi ships an npm-shrinkwrap whose nested entries lack
# integrity hashes (prefetch-npm-deps refuses those); patch them in from the
# registry as documented in the lockfile's sibling README.
{ pkgs }:

let
  # Build one npm meta-package and expose the wrapped bin of its dependency.
  metaNpm = { name, src, npmDepsHash, bin }:
    pkgs.buildNpmPackage {
      pname = name;
      version = "0.0.0";
      inherit src npmDepsHash;
      dontNpmBuild = true;
      nativeBuildInputs = [ pkgs.makeWrapper ];
      # The meta-package itself has no bin; the dependency's bin lands in the
      # project's node_modules/.bin. Wrap it with node on PATH (npm bin stubs
      # use `#!/usr/bin/env node`).
      postInstall = ''
        mkdir -p $out/bin
        makeWrapper "$out/lib/node_modules/${name}/node_modules/.bin/${bin}" \
          "$out/bin/${bin}" --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.nodejs ]}
      '';
    };
in
{
  pi = metaNpm {
    name = "corral-e2e-pi";
    src = ./npm/pi;
    npmDepsHash = "sha256-EM+PHH/BCD50AW2eIsmufy2TYOHOOKf7Hu9f3yoFNSc=";
    bin = "pi";
  };
}
