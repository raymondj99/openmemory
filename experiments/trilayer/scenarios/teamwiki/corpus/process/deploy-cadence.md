# Deploy cadence, revised (May 2026)

The Friday and afternoon freeze is retired: with per-tenant
deployments and automatic rollback, batching releases increased risk
instead of reducing it. New cadence: deploy continuously, one tenant
cohort at a time, with an automatic 30-minute canary per cohort. The
quarter-close billing freeze stays. Supersedes the deploy freeze
policy.
