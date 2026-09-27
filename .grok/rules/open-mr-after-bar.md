# Opening an MR is the green light to merge

Do not create a GitLab MR until the merge bar is green on the **final**
tree. A branch may exist. The MR must not. Starting tests or a ship-image
bake and opening the MR while they run is not done.

The only command that may create an MR is `scripts/open-mr.sh` (it runs
`scripts/assert-mr-ready.sh` first). Never `git push -o merge_request.create`.

If you are about to create or reopen an MR, read `.grok/skills/open-mr/SKILL.md`
and follow it. If the user says an MR was closed, finish the bar — do not
open a replacement yet.
