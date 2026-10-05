# Standalone Mode

Run the production binary without Kubernetes:

```sh
latchkey serve --mode standalone --config examples/local.toml --listen 127.0.0.1:8080
```

The local file uses `version = 1` and defines `[[services]]` entries with an
HTTP MCP `endpoint`, timeout, refresh interval, and optional credential
references. Credential values are read from `value_file` or `value_env`; do
not put them in tracked configuration.

The process exposes only `/mcp`, `/healthz`, and `/readyz`. Readiness is
reported after the source has loaded and the initial reconciliation attempt
has completed. An unavailable downstream does not prevent unrelated services
from being discovered or readiness from becoming healthy.
