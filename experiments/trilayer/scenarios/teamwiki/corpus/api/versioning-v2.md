# API versioning policy, v2

Replaces v1 of this policy. Versioning moves from URL path to a
date-based header (Api-Version: 2026-06-01), pinned per integration
at first call. Rationale: URL majors forced customers into big-bang
migrations; dated pins let them adopt changes one at a time. The 180
day deprecation window is unchanged. /beta/ remains as before.
