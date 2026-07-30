# API versioning policy, v1

Version in the URL path (/v1/). Breaking changes require a new major
version; we support at most two majors concurrently and give 180
days deprecation notice. Additive changes ship without a version
bump. Beta endpoints live under /beta/ with no stability promise.
