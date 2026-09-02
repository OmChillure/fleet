# Partners — credit and trust

Design only. Other people’s unused capacity as a runner kind. Sandboxed.
This MR is placeholder UI + this note. No live lending, no live borrow.

Partners are still **runners**. Tick keeps one assignment path:
`select_assignable_runner`. Do not invent a second spawn path. A partner
host becomes assignable later by extending *who* that wrapper can pick.

---

## Idea

A two-sided pool:

- **Lender** offers idle machines.
- **Borrower** runs a job on someone else’s machine and pays in credit.
- **Fleet** matches, isolates, and takes a cut.

The hard parts are trust (your job on a stranger’s box; a stranger’s
process on yours), credit (what the unit is, who books it), abuse
(mining, exfil, farming), and network (no inbound holes on home PCs).
Those are product decisions. They are not a ledger, a payout rail, or
a new executor.

---

## Credit

**Unit is TBD.** Do not pick hours, CPU-hours, tokens, or job-count in
this MR. There is no meter and no sandboxed partner executor yet. UI
copy says “credit,” not a fake unit.

**Take-rate is a setting**, not a baked constant. Default:

| Share | Party |
| --- | --- |
| **0.9** | Lender |
| **0.1** | Fleet |

When a ledger exists, that split applies to credit earned on a finished
job. Changing the setting does not rewrite history. This MR does not
persist, display as live balances, or enforce the split.

No conversion to money here. See [Out](#out).

---

## Trust

Two tiers. The borrower must see which one they would land on.

| Tier | Who | Default |
| --- | --- | --- |
| **Verified** | Fleet has attested the operator / machine | Eligible once lending is actually on |
| **Open** | Anyone who offered a box | Borrower must opt in; never silent |

Verification is an attestation flag, not a KYC program (KYC is out).
How a host becomes verified is TBD (manual at first). Open is
unidentified beyond the lending workspace.

A job must never land on Open because the borrower did not notice.
Native isolation is forbidden on a partner host — Partners are
**Sandboxed** only.

---

## Disclaimer

The Partners page shows a disclaimer before anything else useful.
It covers at least:

- Work may run on hardware Fleet does not operate.
- No SLA. Fleet can stop the pool at any time ([kill-switch](#abuse--kill-switch)).
- Secrets stay in the control plane; they do not ride the job.
- Credit unit is not final. Ack is not a price or a contract to pay.

**Acknowledging does not enable lending.** It does not enable
borrowing, make a host assignable, open the pool, or create a
balance. Ack is a recorded timestamp only.

Later, lending and borrowing are separate explicit actions
(“offer this machine”, “allow partner runners”). Neither is implied
by the checkbox.

---

## Outbound-only partner agent

Same topology as self-hosted `fleet-host`: create-then-pair, outbound
HTTPS to the control plane, heartbeat + lease poll. **No inbound port**
on the partner box. Fleet does not SSH in.

The partner agent is another host the existing picker can see later.
It is not a new inbound service and not a second spawn path.

---

## Abuse / kill-switch

Kill-switch is first-class even with no ledger:

| Scope | Effect |
| --- | --- |
| Fleet-global | Stop all partner assignment immediately (ops) |
| Per-workspace | Owner disables borrow and/or lend |
| Per-host | Revoke / pause / drain (same idea as worker-host) |

Assignment-side only: `select_assignable_runner` must not pick a
partner host when any of those are on. Drain finishes the current
job and takes no new one. Revoke drops the machine.

Abuse the switch exists for: unexpected egress, volume / farming,
borrower reports, malware on the box. Signals and thresholds are
TBD. The switch does not need payouts or KYC to work.

---

## Assignment

Today the wrapper picks a paired **Native** `fleet-host`, else the
task stays ready. Partners stay off that list until a later MR
extends assignable kinds.

When they join the list:

- Isolation is Sandboxed (not Native).
- Trust tier + borrower opt-in + kill-switch are filters on the
  same wrapper.
- Still one spawn. Still one lease. Still no AWS/k8s fork here.

---

## This MR (placeholder)

`/{workspace_id}/runners/partners` is trust copy: idea, disclaimer,
ack, verified vs open, take-rate default, kill-switch mention.
Ack storage, if any, does not flip lending on.

Cloud placeholders (`/runners/cloud`) are a different kind
(customer keys / Fleet pool). They are not this design.

---

## Out

Do **not** build in this MR (or sneak into the placeholder):

| Out | Why |
| --- | --- |
| Ledger | No balances, transfers, or invoices |
| Payouts | No money, no bank, no crypto rail |
| KYC | Attestation ≠ identity program |
| AWS SDK | Public cloud, not Partners |
| k8s executor | Self-hosted sandbox / later backend; not v1 |

Also out: a second tick/spawn path, Native partner jobs, inbound
ports, and treating ack as “lending is on.”
