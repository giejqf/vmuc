# Homebrew packaging

`vmuc.rb` is the formula template; the tap,
[giejqf/homebrew-vmuc](https://github.com/giejqf/homebrew-vmuc), holds
the rendered copy as `Formula/vmuc.rb` together with the bottles, which
are its release assets. A bottle is the installed keg as a tarball, the
binary already signed with the hypervisor entitlement and the guest
kernel and agent included, so `brew install` downloads that one archive
instead of Rust, LLVM and the sources, and builds nothing.

The tap's CI is Homebrew's own (`brew tap-new`'s workflows):
`tests.yml` runs `brew test-bot` on every pull request on `macos-15` and
`macos-26`, which builds the formula, tests it, pours the bottle it made
and tests that too; `publish.yml` (`brew pr-pull`) uploads those bottles
and commits the `bottle do` block to the tap's `main`. A bottle for
macOS 15 also serves later versions that have none of their own.

## Releasing X.Y.Z

1. Bump the workspace version, update `CHANGELOG.md`, tag `vX.Y.Z` and
   push the tag. `release.yml` builds and checks every asset and opens a
   draft release with `vmuc.rb` rendered for it.
2. Publish the draft. The formula's kernel and agent URLs point at its
   assets, which a draft does not serve.
3. Open a pull request on the tap with the rendered formula:

   ```
   gh repo clone giejqf/homebrew-vmuc && cd homebrew-vmuc
   git switch -c vmuc-X.Y.Z
   gh release download vX.Y.Z -R giejqf/vmuc -p vmuc.rb -O Formula/vmuc.rb --clobber
   git commit -am "vmuc X.Y.Z" && git push -u origin vmuc-X.Y.Z
   gh pr create --fill
   ```

4. Once `brew test-bot` is green on both runners, publish the bottles:
   `gh workflow run publish.yml -R giejqf/homebrew-vmuc -f pull_request=N`.
   The pull request closes with the formula and its bottles on `main`.

The hosted runners cannot run Hypervisor.framework, so the formula's
test checks the signature and `vmuc info`, not a boot; boot the poured
bottle on an Apple silicon Mac before announcing a release.
