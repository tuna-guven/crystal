# Making a release

For the maintainer, every release. A release is one annotated tag on one
commit, built the same way on two machines (CI and the maintainer's),
signed with a YubiKey only if both builds give the same SHA-256, and
published with three files: `crystal`, `SHA256SUMS` and `SHA256SUMS.sig`
([decisions 0067](../decisions.md#0067), [0069](../decisions.md#0069),
[0071](../decisions.md#0071), [0072](../decisions.md#0072)). Users check
them as in [Installation](../guide/installation.md).

```text
checks ──► version commit ──► tag + push ──► CI builds, prints hash
                                                   │
             publish ◄── y? ◄── sign (touch) ◄── local build, hashes equal?
                │                                      │ no
                ▼                                      ▼
        verify as a user                     stop, nothing is signed
```

## 1. Choose the version (SemVer)

`MAJOR.MINOR.PATCH`; the tag is `v` plus the version (`v0.1.0`).
`packaging/publish-release.sh` refuses a tag that does not start with
`v` and a digit.

Crystal's public interface, the part SemVer protects:

- the command line: command names, arguments, flags, exit codes;
- the vault format on disk (ciphertext layout, `VERSION`, `*.gen`, `*.pin`);
- the key and recipient formats a vault depends on.

While Crystal is 0.x:

| Change | Bump | Example |
| --- | --- | --- |
| Breaking: a vault format the previous version cannot read, a command or flag removed or renamed, a changed default that breaks a workflow | MINOR | 0.1.3 → 0.2.0 |
| New feature, backwards compatible | MINOR (PATCH if tiny) | 0.1.0 → 0.2.0 |
| Bug fix, security fix, docs, a dependency update with no change in behaviour | PATCH | 0.1.0 → 0.1.1 |

From 1.0.0 on: breaking → MAJOR, feature → MINOR, fix → PATCH. 1.0.0
comes when the vault format and the command line are promised stable.

- A version is never reused. A mistake found after the tag was pushed
  becomes the next PATCH, not a new tag of the same name (section 8 has
  the one exception).
- A pre-release is `v0.2.0-rc.1`; mark it on GitHub afterwards with
  `gh release edit v0.2.0-rc.1 --prerelease`.
- `crates/zalloc` is internal and not published; its version does not
  follow Crystal's.

## 2. One-time setup

Check before the first release, and again if anything changed:

```sh
ls ~/.ssh/crystal-release ~/.ssh/crystal-release-2   # key handles exist
gh auth status                                        # gh logged in to GitHub
git remote -v                                         # origin is the GitHub repository
```

Test each YubiKey with its key handle (here key 1; the same with
`~/.ssh/crystal-release-2` and the second YubiKey):

```sh
t=$(mktemp)
echo test >"$t"
ssh-keygen -Y sign -f ~/.ssh/crystal-release -n crystal-release "$t"
ssh-keygen -Y verify -f packaging/allowed_signers -I crystal-release \
    -n crystal-release -s "$t.sig" <"$t"            # Good "crystal-release" signature
rm "$t" "$t.sig"
```

- Publish the two key fingerprints outside the repository (a personal
  website, a social profile, a keyserver-like service) before the first
  release: the installation guide tells users to compare
  `allowed_signers` with such a copy. The fingerprints are in the table
  in [Installation](../guide/installation.md).
- Keep the second YubiKey somewhere other than the first.

## 3. Checks before a release

On an up-to-date, clean `main`:

```sh
git switch main && git pull --ff-only && git status   # must be clean
```

1. The checks from `CLAUDE.md`, all passing:

   ```sh
   cargo fmt --all --check
   cargo clippy --all-targets -- -D warnings
   cargo test
   cargo deny check
   cargo audit
   mdbook build
   ```

2. The long property tests (about 50 minutes, release build, a million
   cases per module):

   ```sh
   scripts/long-tests.sh
   ```

3. Reproducibility here: two clean builds in different directories and
   with different Cargo homes give the same hash, and the binary holds no
   home path:

   ```sh
   packaging/check-reproducible.sh                    # ends with "reproducible"
   ```

4. CI is green on the commit to release:

   ```sh
   gh run list --branch main --limit 5                # CI, docs, long tests: success
   ```

5. A manual check of the release binary, built as users get it:

   ```sh
   packaging/build-release.sh
   ```

   Run `target/x86_64-unknown-linux-musl/release/crystal` with a
   throwaway vault: init, unlock (passphrase and hardware key), create,
   edit and view a note, `mv`, `rm`, lock and auto-lock. Then open a copy
   of a vault made by the previous release; this matters most when the
   vault format changed. Run from `target/`, the binary carries no
   `crystal_exec_t` label; to check confinement as well, install it as in
   [Installation](../guide/installation.md) and check the label with
   `ls -Z`.

6. The book matches what ships: the installation guide, the roadmap, the
   decisions.

A failing check is fixed on `main` in an ordinary commit; then section 3
starts again.

## 4. The version commit

Not for 0.1.0, whose version is already in `Cargo.toml`. For every later
release:

```sh
# Cargo.toml: version = "0.2.0"
cargo check                # updates crystal's entry in Cargo.lock
git diff                   # only Cargo.toml and crystal's lines in Cargo.lock
cargo test
git add -A && git commit -m "Release 0.2.0"
git push origin main
gh run watch               # CI on that commit must pass before the tag
```

`Cargo.lock` goes into the same commit: `build-release.sh` builds with
`--locked`, so a stale lock file fails both builds.

## 5. Tag and push

```sh
git tag -a v0.1.0 -m "Crystal 0.1.0"
git show v0.1.0 --no-patch      # the right commit and message
git push origin main v0.1.0
```

The push starts the "Release build" workflow: CI builds the tag with
`packaging/build-release.sh` and prints `release <tag> <commit> sha256
<hash>`. It holds a read-only token and signs, uploads and publishes
nothing. Wait for it:

```sh
gh run list --workflow release-build.yml --limit 1
gh run watch <run id>           # must complete with success
```

## 6. Build, compare, sign, publish

On the tagged commit, with the first YubiKey plugged in:

```sh
git checkout v0.1.0             # a detached HEAD is expected
packaging/publish-release.sh v0.1.0 ~/.ssh/crystal-release
git switch main
```

The script stops at the first problem:

1. The tree is clean, `HEAD` is the tag, and the tag on GitHub is the
   same commit.
2. CI's run for the tag succeeded; the script reads CI's hash from its
   log.
3. It builds the commit here and prints `commit`, `CI` and `here`. If the
   two hashes differ, it stops and nothing is signed.
4. `packaging/sign-release.sh` writes `SHA256SUMS` and signs it (touch
   the YubiKey), then checks the signature against
   `packaging/allowed_signers`.
5. It asks `Publish v0.1.0 with these three files on GitHub? [y/N]`.
   Read the three hashes once more, then answer `y`.
6. `gh release create` uploads the three files, with the commit and the
   hash in the notes.

The second key, `~/.ssh/crystal-release-2`, works the same. A summary of
changes can be added to the notes afterwards with `gh release edit
v0.1.0 --notes-file notes.md`, keeping the generated line with the
commit and the hash.

## 7. Check the release as a user would

In a new, empty directory:

```sh
mkdir ~/verify-crystal && cd ~/verify-crystal
gh release download v0.1.0
git -C <checkout> show v0.1.0:packaging/allowed_signers >allowed_signers
ssh-keygen -Y verify -f allowed_signers -I crystal-release -n crystal-release \
    -s SHA256SUMS.sig <SHA256SUMS       # Good "crystal-release" signature
sha256sum -c SHA256SUMS                 # crystal: OK
```

- The hash in `SHA256SUMS` equals CI's line (`gh run view <run id> --log
  | grep 'release v0.1.0'`) and the one in the release notes.
- The release has exactly three files, the right tag and the right
  commit.
- `file crystal` says statically linked, x86-64.

Then `rm -r ~/verify-crystal`.

## 8. When something goes wrong

| Situation | What to do |
| --- | --- |
| A check in section 3 fails | Fix it on `main`, commit, run section 3 again. Nothing is tagged yet. |
| CI's Release build fails | `gh run view <run id> --log-failed`. A flaky runner: `gh run rerun <run id>`. Wrong code: as for a wrong tag. |
| The hashes differ | Nothing was signed; do not publish. Compare the two builds as decision 0072 did (an unstripped build, `cargo build -v`, the crate hashes). Usual causes: a variable leaking into the build (`RUSTFLAGS`, a wrapper), another toolchain, a path not remapped. |
| Wrong tag, not published | The one exception to never reusing a version, and only if nobody can have fetched it: `git tag -d v0.1.0 && git push origin :refs/tags/v0.1.0`. Otherwise release the fix as the next PATCH. |
| The first YubiKey is lost or broken | Sign with `~/.ssh/crystal-release-2`. Remove key 1 from `packaging/allowed_signers` in a commit, and update the fingerprint table and the copy published elsewhere. |
| A published release is broken | Never replace its files. Say so in its notes (`gh release edit v0.1.0 --notes "Broken: use v0.1.1"`), fix on `main`, release the next PATCH the whole way. |
| A security fix | The same steps, as a PATCH; the notes name the problem without exploit details. |

## 9. Checklist

```text
Release vX.Y.Z
[ ] Version chosen per SemVer (breaking / feature / fix)
[ ] main up to date and clean
[ ] cargo fmt --all --check
[ ] cargo clippy --all-targets -- -D warnings
[ ] cargo test
[ ] cargo deny check
[ ] cargo audit
[ ] mdbook build
[ ] scripts/long-tests.sh            (about 50 minutes)
[ ] packaging/check-reproducible.sh  -> "reproducible"
[ ] Manual check of the release binary, and a vault from the previous release opens
[ ] The book matches the release
[ ] Cargo.toml and Cargo.lock bumped, committed, pushed, CI green   (not for 0.1.0)
[ ] git tag -a vX.Y.Z -m "Crystal X.Y.Z" && git push origin main vX.Y.Z
[ ] Release build run: success
[ ] git checkout vX.Y.Z
[ ] packaging/publish-release.sh vX.Y.Z ~/.ssh/crystal-release
      CI hash equals here hash, touch the YubiKey, y
[ ] git switch main
[ ] Downloaded release checked: Good signature, crystal: OK, hash equals CI's
[ ] (first release only) key fingerprints published outside the repository
```
