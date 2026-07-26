# Capture And Review

## Goal

Assisted capture turns memory from a manual command into a reviewed
workflow. The product should help users notice durable facts, but it
must not silently pollute memory.

Default rule: candidate memories wait for user approval.

## Candidate Categories

Initial categories:

- User preference.
- Project decision.
- Correction or mistake to avoid.
- Release/deployment procedure.
- Tooling gotcha.
- Architecture decision.
- Security/privacy instruction.

These categories map to review filters and future auto-approve policy.

## Source Model

Every candidate records:

- Client.
- Workspace.
- Profile.
- Source URI.
- Source span when available.
- Capture session.
- Extraction method.
- Proposed entity.
- Proposed entity type.
- Proposed memory tier.
- Confidence.
- Reason.
- Duplicate candidates.

Source spans should point back to transcript lines or local files when
the source is stable. If the source cannot be reopened reliably, store a
small redacted excerpt only when privacy rules allow it.

## Pipeline

```text
source adapter
  -> capture session
  -> text normalization
  -> candidate extraction
  -> duplicate check
  -> suppression check
  -> review inbox
  -> approved memory write
```

The extraction pipeline is isolated from recall/search hot paths.
Failures mark the capture session failed and leave existing memory
untouched.

## Source Adapters

Initial adapters should favor stable local data:

- MCP tool calls made through openmemory.
- CLI `remember` and correction-like commands.
- Local transcript/session files only where format and location are
  stable enough to parse.

Do not build brittle scrapers for private app internals without a clear
support boundary. If a client changes format often, treat it as a spike
or omit it from V1.

## Extraction

Start with rule-based extraction:

- Explicit "remember that" language.
- Correction phrases such as "do not do X again".
- Preference phrases.
- Project decision markers.
- Release/deployment steps.
- Security/privacy directives.

Optional LLM extraction:

- Disabled by default.
- Explicitly configured.
- Shows provider and network/local state in settings.
- Does not send transcript content to a remote provider unless the user
  opted in for that provider.

## Deduplication

Before showing a candidate:

- Compare proposed entity against existing normalized entities.
- Compare proposed content against active observations.
- Check near-duplicate candidates already in inbox.
- Prefer editing an existing memory over creating a duplicate when the
  semantic intent is the same.

The inbox should show duplicate warnings rather than hiding ambiguous
cases.

## Review Decisions

States:

- `pending`
- `approved`
- `rejected`
- `edited`
- `suppressed`
- `expired`

Approving writes memory through the same engine path used by CLI/MCP.
Rejecting stores the decision locally for noise reduction. Editing
updates the candidate first, then approves.

## Suppressions

Suppressions reduce repeated low-value suggestions.

Suppression scopes:

- Exact source.
- Client.
- Workspace.
- Category.
- Pattern.

Suppressions are local product data. They should be inspectable and
removable from settings or the inbox.

## Auto-Approve Policy

Auto-approve is post-beta unless the reviewed workflow proves reliable.
If added:

- Off by default.
- Narrow by category and workspace.
- Shows recent auto-approved memories.
- Easy to disable.
- Never enabled for security/privacy instructions by default.

## Acceptance Criteria

- No inferred candidate persists without approval by default.
- Every approved candidate records provenance.
- Review inbox explains why the candidate was proposed.
- Rejected candidates reduce future repeated noise.
- Candidate extraction works offline in the default configuration.
- Capture failures do not corrupt memory or block recall/search.
- Bulk actions are undoable where storage model allows it.

