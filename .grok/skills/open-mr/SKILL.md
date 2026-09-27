---
name: open-mr
description: >
  Create a GitLab merge request only after the merge bar is green on the
  final tree. Opening an MR is the green light to merge — if a human sees
  an MR they will merge it. Use when the user says create MR, open MR,
  open a merge request, put this in an MR, ship through an MR, GitLab MR,
  merge_request.create, glab mr create, gh pr create, "MR was closed",
  reopen the MR, or /open-mr. Also use before any git push -o merge_request.
---

# Open MR (after the merge bar)

Opening a GitLab MR **is** merge approval. A branch may exist. The MR must not,
until every merge requirement is done on the **final** tree. Starting a bake
or `just test` and opening the MR while it runs is not done.

## Hard rule

The only command that may create an MR is:

```bash
scripts/open-mr.sh
```

Never run `git push -o merge_request.create`, `glab mr create`, `gh pr create`,
or `POST /api/v4/projects/.../merge_requests`. A PreToolUse hook denies those.
`git push gitlab HEAD` (branch only, no create option) is fine.

If the user says the previous MR was closed, do **not** open a replacement.
Finish the bar, then `scripts/open-mr.sh`.

## Steps

1. Finish every edit. `scripts/tree-id.sh` is the identity of *this* tree.
2. `just test` and `just test-integration` green on that tree. Then:
   `scripts/record-mr-bar.sh tests`
3. Ship images on **that** tree-id (not an older SHA):

   ```bash
   id="$(scripts/tree-id.sh)"
   docker build --build-arg FLEET_GIT_SHA="$id" -t "agent-fleet-api:$id" -f Dockerfile .
   docker build --build-arg FLEET_GIT_SHA="$id" -t "agent-fleet-ui:$id" -f Dockerfile.ui .
   scripts/record-mr-bar.sh images
   ```

4. If this is not a small tweak (typo, comment nit, import order, one-line
   docs): film `HUMAN_TEST_SCOPE.md`, fill **Film review**, then
   `scripts/record-mr-bar.sh film`.
   Small tweak only: `scripts/record-mr-bar.sh small-tweak "reason"`.
5. `scripts/assert-mr-ready.sh` must exit 0. A new commit or dirty tree
   changes tree-id and drops every stamp — redo the bar.
6. Only then: `scripts/open-mr.sh`.

Merge-bar details live in `AGENTS.md` (Merge requests and review). Do not
restate them here. Do not open the MR and "let tests finish in the background."
