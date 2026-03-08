# Cogwheel DNS

Cogwheel DNS is a Rust-native DNS adblock platform with a Docker server backend, safe blocklist updates, and a background real-time classifier.

## Current Scope

- Phase 1: monorepo foundation and shared infrastructure
- Phase 2: DNS backend MVP with health endpoints and Docker packaging
- Phase 3: safe blocklist ingestion, verification, and atomic ruleset activation

## Local Development

```bash
cargo fmt --all
cargo clippy --workspace --all-targets --all-features
cargo test --workspace
cargo run -p cogwheel-server
```

## Configuration

The server reads settings from environment variables with the `COGWHEEL_` prefix.

- `COGWHEEL_SERVER__HTTP_BIND_ADDR`
- `COGWHEEL_SERVER__DNS_UDP_BIND_ADDR`
- `COGWHEEL_SERVER__DNS_TCP_BIND_ADDR`
- `COGWHEEL_STORAGE__DATABASE_URL`
- `COGWHEEL_UPSTREAM__SERVERS`

## Design Notes

- The DNS hot path stays deterministic and LLM-independent.
- Blocklist updates are staged and atomically promoted.
- Unsafe or malformed list updates never replace the active ruleset.
