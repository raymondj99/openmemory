# Decision: API authentication

API requests authenticate with scoped bearer tokens minted per
integration, rotated every 90 days. We rejected per-user API keys
(no scoping) and mTLS (operational burden on customers). Tokens are
hashed at rest; the raw value is shown exactly once at mint time.
