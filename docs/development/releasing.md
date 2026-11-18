# Making a release

For the maintainer. A release is one tagged commit, built the same way on
two machines, signed with a YubiKey and published with three files:
`crystal`, `SHA256SUMS` and `SHA256SUMS.sig` ([decisions
0067](../decisions.md#0067), [0069](../decisions.md#0069),
[0071](../decisions.md#0071), [0072](../decisions.md#0072)). Users check
them as in [Installation](../guide/installation.md).

1. All checks green (CLAUDE.md), `scripts/long-tests.sh` passed, the
   version in `Cargo.toml` set and committed.
2. Tag and push:

   ```sh
   git tag -a v0.1.0 -m "Crystal 0.1.0"
   git push origin main v0.1.0
   ```

   The push starts the "Release build" workflow: CI builds the tag with
   `packaging/build-release.sh` and prints `release <tag> <commit> sha256
   <hash>`. It holds a read-only token and signs, uploads and publishes
   nothing.
3. When it has finished, on the tagged commit:

   ```sh
   packaging/publish-release.sh v0.1.0 ~/.ssh/crystal-release
   ```

   The script refuses unless the tree is clean, `HEAD` is the tag, the
   tag on GitHub is the same commit and CI's run for it succeeded. It
   builds the commit here and compares the hash with CI's; only if they
   are equal does it run `packaging/sign-release.sh` (touch the YubiKey),
   and after a `y` it runs `gh release create` with the three files, the
   commit and the hash in the notes. The second key,
   `~/.ssh/crystal-release-2`, works the same.
4. Check the published release as a user would: download it into an
   empty directory and run the two commands from the installation guide.

If the hashes differ, nothing is signed: compare the two builds before
anything else (decision 0072 shows how such a difference was found).
