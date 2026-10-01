# Changelog

Latchkey does not maintain a hand-edited changelog in this repository.

The cumulative, Dojo-style changelog is a **generated release artifact**:

- Produced by `scripts/release.py changelog --commit <sha> --output <path>`
  from the immutable `.changes/*.toml` fragments (contract:
  [.changes/README.md](.changes/README.md)).
- Published with every release alongside the source archive, binary, OCI
  image, checksums, and per-release notes (names and rules:
  [docs/releases.md](docs/releases.md)).
- **Never committed back to `master`.** Release automation therefore cannot
  create a commit loop or a shared-file merge conflict; this pointer file is
  the only changelog content tracked in git.

Release history: <https://github.com/blogle/latchkey/releases>
