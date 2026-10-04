# Standalone Protocol Decisions

The public endpoint is `/mcp` and uses the official `rmcp` 3.0.1
Streamable HTTP service. The server uses `NeverSessionManager` and explicitly
disables legacy session mode, so requests do not depend on replica-local
session state.

The negotiated protocol versions are `2026-07-28`, `2025-11-25`, and
`2025-06-18`. The advertised tool list contains exactly `search` and `exec`;
their schemas are generated from the frozen request contracts.

Standalone configuration is TOML (`version = 1`). Service credentials are
referenced by relative files or startup environment variable names and are
never stored in the configuration file. Header entries use `name` plus exactly
one of `value_file` or `value_env`; file references resolve relative to this
configuration file.
