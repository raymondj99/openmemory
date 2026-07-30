"""Three more scenario corpora: novelist, freelancer, teamwiki.

teamwiki additionally declares a corrections list: pairs of
(old_doc, new_doc) where the new decision supersedes the old one.
`apply_corrections.py` expresses each correction through the product
surfaces available today (forget the stale gloss, remember an
OUTDATED marker with source=correction, add a supersedes relation),
and the eval runs before and after to measure what correction
actually changes per route.
"""

import json
import os

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "scenarios"))

N, F, W = {}, {}, {}

# --------------------------------------------------------------- novelist --
N["book1/outline.md"] = """# Book One: The Cinder Road — outline

Orphaned mapmaker Mara Venn discovers her survey markers are moving on
their own. The trail leads to the Ember Court, where she is taken on
as apprentice to the cartographer-general Elias Thorn. Midpoint: the
mirror blade is found sealed inside the archive vault. Climax: Mara
chooses to redraw the border and the town of Grayhollow burns.
"""

N["book1/revision-notes.md"] = """# Book One revision notes

Third draft. Cut the twin brother subplot entirely; his one essential
scene moves to Thorn. The vault sequence drags: fold the two archive
visits into one night. Beta readers keep missing that the moving
markers are Sela's doing, so seed one earlier clue in chapter four.
"""

N["book1/characters/mara-venn.md"] = """# Character: Mara Venn

Mapmaker's apprentice, nineteen, stubborn about measured truth over
official truth. Wants: a chartered survey of her own. Wound: her
mother vanished on the Cinder Road when Mara was nine. Voice: dry,
precise, counts steps when anxious. Arc in book one: learns maps are
arguments, not facts.
"""

N["book1/characters/elias-thorn.md"] = """# Character: Elias Thorn

Cartographer-general of the Ember Court. Trained Sela Venn before her
disappearance; takes Mara on out of guilt he never names. Keeps the
mirror blade's existence from the Court. Speaks in committee-safe
sentences that say nothing; his hands shake when he lies.
"""

N["book2/outline.md"] = """# Book Two: The Salt Meridian — outline

The redrawn border has consequences: salt caravans now cross a desert
that legally does not exist. Mara, exiled from the Court, surveys the
meridian for the caravan guilds. A rival mapmaker, Corin Ashe,
carries a forged copy of her Grayhollow chart. Midpoint reveal: Sela
Venn is alive and drawing the desert from the inside. Ending: Mara
breaks the mirror blade rather than let Thorn use it on the guild
routes.
"""

N["book2/revision-notes.md"] = """# Book Two revision notes

Second draft. The guild politics chapters read like minutes; give
each guildmaster one concrete want. Corin's forgery needs to be
discovered by a reader-visible mistake, not by authorial fiat: use
the north-arrow habit from book one. Sela's chapters switch to
present tense; keep that, it unsettles.
"""

N["book2/characters/corin-ashe.md"] = """# Character: Corin Ashe

Rival mapmaker under guild contract. Not a villain: he believes
copied maps save lives because originals get people killed. Left-
handed, which is how his forged Grayhollow chart betrays him: the
north arrow leans the wrong way. By the end he burns his own license
to testify for Mara.
"""

N["book3/outline.md"] = """# Book Three: The Unwritten Coast — outline

With the mirror blade broken, every map it ever touched begins to
revert. Mara, Sela, and Corin race the reversion west to the coast
that has never been charted. Thorn, dying, follows with the Court
fleet. The ending inverts book one: Mara refuses to draw the coast at
all, leaving one place in the world unwritten, and Grayhollow is
resettled on its true site.
"""

N["book3/revision-notes.md"] = """# Book Three revision notes

First draft in progress. The mother-daughter chapters are the spine;
everything at sea can compress. Thorn needs one scene of genuine
kindness before the end or his death lands as relief. Decide whether
the epilogue names the coast: current answer is no, and the last line
stays 'here the survey ends.'
"""

N["worldbuilding/mirror-blade.md"] = """# The mirror blade

A surveyor's instrument from the first charter era: whatever border
it is drawn along becomes legally and then physically true. It does
not create land; it moves error. Every use displaces a real place
into the margins, which is what happened to the original Grayhollow.
Sealed in the Ember Court archive after the Partition Survey.
Destroyed by Mara Venn at the end of the Salt Meridian events.
"""

N["worldbuilding/ember-court.md"] = """# The Ember Court

The chartered authority over maps, borders, and surveys. Its archive
holds every official chart since the Partition Survey; unofficial
copies are contraband. Governed by the cartographer-general, a post
held by Elias Thorn, who trained both Sela Venn and later her
daughter Mara. Court law: the map outranks the ground.
"""

N["worldbuilding/sela-venn.md"] = """# Character: Sela Venn

Mara's mother. The finest surveyor of her generation and the first to
discover what the mirror blade displaces. Faked her disappearance on
the Cinder Road to search for the erased original Grayhollow from
inside the unmapped margins. Her moving survey markers in book one
were messages only a mapmaker would notice.
"""

N["worldbuilding/grayhollow.md"] = """# Grayhollow

A border town that exists twice. The chartered Grayhollow burned when
Mara redrew the border at the end of book one. The original
Grayhollow, displaced by the mirror blade generations earlier,
persists unmapped in the margins; finding and resettling it is the
series' final image.
"""

# ------------------------------------------------------------- freelancer --
F["acme/contract.md"] = """# Acme Robotics — contract

Scope: brand refresh and marketing site rebuild. Fixed fee 24,000,
half on signature, half on launch. Timeline: eight weeks from kickoff.
Two revision rounds included; further rounds billed at the standard
day rate. IP transfers on final payment. Termination requires
fourteen days written notice.
"""

F["acme/brief.md"] = """# Acme Robotics — project brief

Acme sells warehouse picking arms to mid-size logistics firms. The
current site reads like a patent filing; buyers are operations
directors, not engineers. Voice target: confident, concrete, zero
jargon. Deliverables: messaging framework, visual identity refresh,
five-page marketing site, case study template.
"""

F["acme/meeting-notes.md"] = """# Acme Robotics — meeting notes

## Kickoff, 2026-06-03
Attendees: Dana (Acme CMO), me, Priya Nair (subcontracted motion
design). Decision: lead with the 40% mispick-reduction case study.

## Week 3 review, 2026-06-24
Dana wants the demo video above the fold. Priya to storyboard.
Risk flagged: their legal review adds two weeks to any claim we print.
"""

F["acme/invoice-2026-06.md"] = """# Invoice — Acme Robotics, June 2026

Invoice A-114. Signature milestone per contract: 12,000. Payment
terms net 30, due 2026-07-05. Late fee 1.5% monthly. Remit to the
usual account; PO number ACM-2231 must appear on the transfer.
Status: paid 2026-07-01.
"""

F["bluesky/contract.md"] = """# BlueSky Dental — contract

Scope: patient-booking web app, design plus front-end build. Time and
materials at the standard day rate, estimated 30 to 38 days, invoiced
monthly. BlueSky provides the booking API and HIPAA-reviewed copy.
Either party may pause with seven days notice; IP assigns per paid
invoice.
"""

F["bluesky/brief.md"] = """# BlueSky Dental — project brief

Three-clinic dental group losing bookings to phone-tag. Goal: patients
book, reschedule, and get reminders without calling. Constraints: must
match their existing brand, WCAG AA, and the booking API's odd
fifteen-minute slot model. Success metric: 40% of bookings online by
Q4.
"""

F["bluesky/meeting-notes.md"] = """# BlueSky Dental — meeting notes

## Kickoff, 2026-05-12
Attendees: Dr. Okafor, practice manager Lena, me. Decision: build
reschedule flow before new-patient flow; it is 70% of call volume.

## API review, 2026-06-09
Their vendor's slot API cannot hold a tentative booking. Workaround:
optimistic lock client-side, confirm within 90 seconds. Lena approved
the compromise.
"""

F["bluesky/invoice-2026-06.md"] = """# Invoice — BlueSky Dental, June 2026

Invoice B-221. Eleven days at day rate: 8,800. Terms net 15, due
2026-06-30. Includes the reschedule-flow build and the API workaround
spike from the June 9 review. Status: overdue as of 2026-07-15;
reminder sent, Lena confirms payment run on the 25th.
"""

F["copper/contract.md"] = """# Copper Kettle Coffee — contract

Scope: packaging redesign for four retail SKUs plus a print style
guide. Fixed fee 9,500. Three concepts, one selected direction, two
revision rounds. Print production handled by their printer; I approve
proofs. Kill fee: 30% if cancelled after concept review.
"""

F["copper/brief.md"] = """# Copper Kettle Coffee — project brief

Regional roaster moving from farmers markets into grocery shelves.
Shelf problem: their kraft-paper look disappears next to the big
brands. Keep the hand-drawn kettle mark; everything else can change.
Mandatory: roast date front of pack, compostable film supplier's
print constraints (two spot colors max).
"""

F["copper/meeting-notes.md"] = """# Copper Kettle Coffee — meeting notes

## Concept review, 2026-06-17
Attendees: Sam and Rivka (owners), me, Priya Nair (illustration pass
on the kettle mark). Selected concept two, the block-print direction.
Rivka wants the origin story on the back panel; trimmed to 60 words.

## Proof check, 2026-07-10
Printer's first proof shifted the copper spot color; rejected, new
proof due before the 24th.
"""

F["copper/invoice-2026-06.md"] = """# Invoice — Copper Kettle Coffee, June 2026

Invoice C-108. Concept milestone per contract: 4,750. Terms net 30,
due 2026-07-17. PO not required. Status: paid 2026-07-08 with a nice
note about the block-print direction.
"""

F["ops/rates.md"] = """# Standard rates and terms

Day rate 800 for design and front-end, 950 for rush work inside five
business days. Fixed-fee projects priced at estimated days times day
rate plus 15% contingency. Subcontractors: Priya Nair for motion and
illustration at 500 per day, billed through me with a 10% handling
margin. Annual rate review each January.
"""

F["ops/pipeline.md"] = """# Pipeline and capacity

Committed: Acme launch through August; BlueSky build through
September at three days per week. Copper Kettle wraps on proof
approval. Prospects: the Northgate gym rebrand (proposal sent 07-02,
warm) and a referral from Dr. Okafor to an orthodontics group (call
scheduled). Capacity opens roughly mid-September.
"""

# --------------------------------------------------------------- teamwiki --
W["decisions/2026-02-database.md"] = """# Decision: primary datastore (February 2026)

We choose Postgres for the platform's primary datastore. Rationale:
the team knows it, managed hosting is cheap at our scale, and the
relational model fits the billing tables. Revisit if the embedded
analytics product ships, since that workload is append-heavy and
read-mostly.
"""

W["decisions/2026-06-database.md"] = """# Decision: primary datastore, revised (June 2026)

We are moving the platform's primary datastore from Postgres to
SQLite-per-tenant. The February decision predates the single-tenant
pivot: each customer now gets an isolated deployment, and one shared
Postgres was pure overhead. Migration owner: Jonah. Target: all
tenants migrated by end of Q3. Supersedes the February datastore
decision.
"""

W["decisions/2026-01-api-auth.md"] = """# Decision: API authentication

API requests authenticate with scoped bearer tokens minted per
integration, rotated every 90 days. We rejected per-user API keys
(no scoping) and mTLS (operational burden on customers). Tokens are
hashed at rest; the raw value is shown exactly once at mint time.
"""

W["oncall/rotation-q1.md"] = """# On-call rotation, Q1 2026

Primary rotates weekly among Jonah, Priya, and Wen, with Ana as
permanent escalation. Handoff is Monday 10:00 with a written summary
in the channel. Pages outside business hours only for sev-1 and
sev-2; everything else waits for morning triage.
"""

W["oncall/rotation-q3.md"] = """# On-call rotation, Q3 2026

New rotation from July: primary rotates weekly among Priya, Wen, and
the two new hires (Marta, Deniz); Jonah leaves the rotation while he
owns the datastore migration. Ana remains escalation. Handoff moves
to Monday 09:30. This replaces the Q1 rotation document.
"""

W["api/versioning-v1.md"] = """# API versioning policy, v1

Version in the URL path (/v1/). Breaking changes require a new major
version; we support at most two majors concurrently and give 180
days deprecation notice. Additive changes ship without a version
bump. Beta endpoints live under /beta/ with no stability promise.
"""

W["api/versioning-v2.md"] = """# API versioning policy, v2

Replaces v1 of this policy. Versioning moves from URL path to a
date-based header (Api-Version: 2026-06-01), pinned per integration
at first call. Rationale: URL majors forced customers into big-bang
migrations; dated pins let them adopt changes one at a time. The 180
day deprecation window is unchanged. /beta/ remains as before.
"""

W["process/deploy-freeze.md"] = """# Deploy freeze policy

No production deploys after 15:00 local or on Fridays, and a full
freeze during the last week of each quarter for billing close. Break
glass requires two approvals and a rollback plan pasted in the
channel before the deploy starts.
"""

W["process/deploy-cadence.md"] = """# Deploy cadence, revised (May 2026)

The Friday and afternoon freeze is retired: with per-tenant
deployments and automatic rollback, batching releases increased risk
instead of reducing it. New cadence: deploy continuously, one tenant
cohort at a time, with an automatic 30-minute canary per cohort. The
quarter-close billing freeze stays. Supersedes the deploy freeze
policy.
"""

W["process/incident-review.md"] = """# Incident review process

Every sev-1 and sev-2 gets a written review within five working
days: timeline, contributing causes (never a single root cause),
what surprised us, and at most three action items with owners.
Blameless means we name decisions and conditions, not people.
Reviews are filed next to this page and linked from the incident
channel.
"""

W["infra/backups.md"] = """# Backups and restore drills

Tenant datastores snapshot hourly, retained 30 days, with a weekly
cross-region copy. The restore drill runs monthly: pick a random
tenant snapshot, restore to a scratch environment, run the billing
reconciliation check against production. A backup that has not been
restored is a rumor, not a backup.
"""

SCENARIOS = {
    "novelist": {
        "corpus": N,
        "projects": {
            "book1": "Book One, The Cinder Road: Mara Venn's apprenticeship and the burning of Grayhollow.",
            "book2": "Book Two, The Salt Meridian: exile, the caravan guilds, Sela alive, the mirror blade broken.",
            "book3": "Book Three, The Unwritten Coast: the reversion, the uncharted coast, Grayhollow resettled.",
            "worldbuilding": "Series bible: the mirror blade, the Ember Court, recurring characters and places.",
        },
        "project_relations": [
            ("book1", "precedes", "book2"),
            ("book2", "precedes", "book3"),
            ("worldbuilding", "related_to", "book1"),
            ("worldbuilding", "related_to", "book2"),
            ("worldbuilding", "related_to", "book3"),
        ],
        "representatives": ["outline.md"],
    },
    "freelancer": {
        "corpus": F,
        "projects": {
            "acme": "Client Acme Robotics: brand refresh and marketing site, fixed fee.",
            "bluesky": "Client BlueSky Dental: patient booking app, time and materials.",
            "copper": "Client Copper Kettle Coffee: packaging redesign, fixed fee.",
            "ops": "Business operations: rates, subcontractors, pipeline.",
        },
        "project_relations": [
            ("acme", "related_to", "ops"),
            ("bluesky", "related_to", "ops"),
            ("copper", "related_to", "ops"),
        ],
        "representatives": ["brief.md", "rates.md"],
    },
    "teamwiki": {
        "corpus": W,
        "projects": {
            "decisions": "Architecture and technology decisions, dated.",
            "oncall": "On-call rotations and escalation.",
            "api": "Public API policies.",
            "process": "Engineering process policies.",
            "infra": "Infrastructure runbooks and policies.",
        },
        "project_relations": [
            ("decisions", "related_to", "infra"),
            ("process", "related_to", "oncall"),
        ],
        "representatives": [],
        "corrections": [
            {"old": "decisions/2026-02-database.md", "new": "decisions/2026-06-database.md"},
            {"old": "oncall/rotation-q1.md", "new": "oncall/rotation-q3.md"},
            {"old": "api/versioning-v1.md", "new": "api/versioning-v2.md"},
            {"old": "process/deploy-freeze.md", "new": "process/deploy-cadence.md"},
        ],
    },
}


def main():
    for name, sc in SCENARIOS.items():
        base = os.path.join(ROOT, name, "corpus")
        for path, content in sc["corpus"].items():
            full = os.path.join(base, path)
            os.makedirs(os.path.dirname(full), exist_ok=True)
            with open(full, "w") as f:
                f.write(content)
        meta = {k: v for k, v in sc.items() if k != "corpus"}
        with open(os.path.join(ROOT, name, "scenario.json"), "w") as f:
            json.dump(meta, f, indent=2)
        print(f"{name}: {len(sc['corpus'])} files")


if __name__ == "__main__":
    main()
