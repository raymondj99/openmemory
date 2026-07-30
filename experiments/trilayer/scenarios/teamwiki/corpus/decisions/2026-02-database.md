# Decision: primary datastore (February 2026)

We choose Postgres for the platform's primary datastore. Rationale:
the team knows it, managed hosting is cheap at our scale, and the
relational model fits the billing tables. Revisit if the embedded
analytics product ships, since that workload is append-heavy and
read-mostly.
