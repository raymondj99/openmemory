# Backups and restore drills

Tenant datastores snapshot hourly, retained 30 days, with a weekly
cross-region copy. The restore drill runs monthly: pick a random
tenant snapshot, restore to a scratch environment, run the billing
reconciliation check against production. A backup that has not been
restored is a rumor, not a backup.
