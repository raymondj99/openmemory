# Legacy profile compatibility fixture

`legacy-profile-v2.json` is a permanent, inspectable workload for the
legacy-compatible one-domain profile path.

Provenance:

- created for Phase 0 at source revision
  `bcd10fd3f736290c536ac94daf5a9014ef711828`;
- content is a manual, sanitized paraphrase of this repository's
  `docs/storage.md`, `docs/context-engine.md`, and `docs/watcher.md`;
- it contains no user data, credentials, absolute paths, network content, or
  prototype-specific schema;
- entity names intentionally exercise project, concept, and tool types plus
  relations and multi-observation recall.

The integration test imports this data through public production graph APIs,
opens it through the byte-compatible one-domain `DomainStore`, migrates it to
four real SQLite/index domain families and back, and verifies semantic counts
and recall after every reopen. No fixture name or field selects a special
production branch.
