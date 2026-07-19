# Test-only packages for the VM e2e smoke tests.
#
# pi is the published npm package `@earendil-works/pi-coding-agent`, built from
# its release tarball with buildNpmPackage. pi bundles an npm-shrinkwrap.json
# that pins its dependency tree, but references its own sibling packages
# (`@earendil-works/pi-{tui,ai,agent-core}`) WITHOUT integrity hashes, which
# breaks any offline `npm ci`. So we vendor a copy of that shrinkwrap with the
# three integrities patched in from the registry (nix/tests/npm/pi/) and swap
# it into the source before the deps are fetched, so the whole tree resolves
# from the pinned, offline npm cache.
#
# Updating the pin: bump `piVersion`, refresh `piTarballHash`
# (`nix-prefetch-url <tarball> | xargs nix hash to-sri --type sha256`),
# regenerate the patched shrinkwrap (extract npm-shrinkwrap.json from the
# tarball, fill the three missing @earendil integrities from the registry --
# see nix/tests/npm/pi/README.md), and refresh `npmDepsHash`
# (`nix run nixpkgs#prefetch-npm-deps -- ./npm-shrinkwrap.json`).
{ pkgs }:

let
  piVersion = "0.80.10";
  piTarball = pkgs.fetchurl {
    url = "https://registry.npmjs.org/@earendil-works/pi-coding-agent/-/pi-coding-agent-${piVersion}.tgz";
    hash = "sha256-nydxcRuNTruNWeMXcCarQXv8bK8NQpaljedBtB5NnBw=";
  };

  # The tarball contents with the integrity-patched shrinkwrap swapped in and
  # devDependencies stripped, used as the build source so both the deps FOD and
  # the build read the fixed, production-only tree. pi ships prebuilt dist/ and
  # we run no build, so its devDependencies (e.g. @types/cross-spawn) are never
  # needed -- and leaving them in makes offline `npm ci` probe the registry for
  # them (they are absent from the production shrinkwrap and the cache).
  piSrc = pkgs.runCommand "pi-coding-agent-src-${piVersion}"
    { nativeBuildInputs = [ pkgs.jq ]; } ''
    mkdir -p $out
    tar xzf ${piTarball} --strip-components=1 -C $out
    # Verbatim so it stays byte-identical to the deps FOD's copy (the npm ci
    # consistency check diffs them). The vendored shrinkwrap is already
    # production-only, so nothing to strip here.
    cp ${./npm/pi/npm-shrinkwrap.json} $out/npm-shrinkwrap.json
    # Drop devDependencies from package.json only: with them present, npm ci
    # builds an ideal tree that probes the registry for dev-only packages
    # (e.g. @types/cross-spawn) absent from the production cache.
    jq 'del(.devDependencies)' $out/package.json > $out/package.json.tmp
    mv $out/package.json.tmp $out/package.json
  '';
in
{
  pi = pkgs.buildNpmPackage {
    pname = "pi-coding-agent";
    version = piVersion;
    src = piSrc;
    npmDepsHash = "sha256-43Eq0PZRWYKij5TDbQaZ3YEuAnRRHWpeg6DQ4kG7xV0=";
    # pi ships prebuilt dist/; there is no build step to run. devDependencies
    # are stripped in piSrc so the offline npm ci sees only the runtime tree.
    dontNpmBuild = true;
    # bin `pi` -> dist/cli.js is declared in package.json; buildNpmPackage links
    # it into $out/bin and patches the node shebang.
  };
}
