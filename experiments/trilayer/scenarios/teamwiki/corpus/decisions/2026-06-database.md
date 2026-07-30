# Decision: primary datastore, revised (June 2026)

We are moving the platform's primary datastore from Postgres to
SQLite-per-tenant. The February decision predates the single-tenant
pivot: each customer now gets an isolated deployment, and one shared
Postgres was pure overhead. Migration owner: Jonah. Target: all
tenants migrated by end of Q3. Supersedes the February datastore
decision.
