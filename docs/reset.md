# Fresh-baseline reset record

## Preserved state

- Repository: https://github.com/blogle/latchkey
- Previous master: `4e270e493db9939d8e31916aa02019b89864101d`
- Backup branch: `backup/pre-mvp-reset-2026-09-30`
- Annotated tag: `pre-mvp-reset-2026-09-30`

The branch and tag were pushed atomically and verified on GitHub before preparing the replacement. Both resolve to the previous master commit, preserving all four historical commits.

## Reset method

Replace the active tree in a normal commit on master. This gives a fresh specification baseline and retains ancestry; no force push or orphan history is needed. Retain the license; replace old code, deployment manifests, CI, outdated specifications and agent guidance with the supplied requirements, review and plan. The supplied attachment is stored byte-for-byte as `docs/spec.md`.

Before publication: check the replacement tree, whitespace, relative documentation links, source attachment equality, preserved license, ancestry and remote master. Push normally so a concurrent remote change rejects publication instead of overwriting it. Verify remote master and both backup refs after publication.

This is a repository reset only. No images are published and no cluster resources, ingress routes, Nexus or compatibility service are changed. The new tree is deliberately not deployable until implementation and acceptance work is complete.

## Rollback

For inspection, check out the backup branch in a separate clone/worktree. To restore the old active contents without rewriting shared history, revert the single commit titled `Reset to the supplied Latchkey MVP requirements baseline` on master and push normally. If later changes exist, review conflicts and their consequences before pushing. The backup branch/tag remain the exact pre-reset reference.

Do not delete the backup refs during implementation. Historical source links in the review use immutable commit IDs.
