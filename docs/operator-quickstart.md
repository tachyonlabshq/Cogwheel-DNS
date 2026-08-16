# Operator Quick Start

A one-page reference for people who run a Cogwheel node.

**[DEPLOYMENT.md](../DEPLOYMENT.md) is the full guide** — installation, the
port-53 conflict, networking modes, upgrades, backup/restore, troubleshooting
and uninstall all live there. This page is the short version.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/thekozugroup/Cogwheel-DNS/main/scripts/install.sh | sudo sh
```

Then point your router's DNS at the address the installer prints.

Other paths: Docker Compose (`cp .env.example .env && docker compose up -d`)
and native systemd (`sudo ./scripts/install-native.sh`). See
[DEPLOYMENT.md §1](../DEPLOYMENT.md#1-choosing-an-install-method).

## Verify

```sh
sh scripts/verify-install.sh
```

Checks liveness, readiness, metrics, the web UI, an allowed lookup, a blocked
lookup, DNS over TCP, and that state survives a restart. Exits non-zero on
failure, so it is safe to run from cron.

## The one thing that goes wrong

Port 53. On most Linux hosts `systemd-resolved` holds `127.0.0.53:53`.

```sh
sudo ss -lnptu '( sport = :53 )'     # who has it
sudo ./scripts/install.sh --fix-port-53
```

That disables the stub listener *and* repairs `/etc/resolv.conf`, which would
otherwise leave the host with no working resolver. Full detail and the manual
equivalent: [DEPLOYMENT.md §8.1](../DEPLOYMENT.md#81-port-53-is-already-in-use).

## Networking decides a feature

Per-device block profiles key on the DNS client's source IP. Host networking
preserves it; Docker bridge networking often rewrites it to the bridge gateway,
which silently collapses every device into one. Default to host networking and
verify with a query from a second machine —
[DEPLOYMENT.md §5](../DEPLOYMENT.md#5-networking-host-vs-bridge-and-why-it-decides-a-feature).

## Deployment profiles

| Profile | HTTP bind | DNS bind | Use for |
|---|---|---|---|
| `dev` | `127.0.0.1:30080` | `127.0.0.1:30053` | local development |
| `home` | `0.0.0.0:8080` | `0.0.0.0:5353` | household node (the image overrides DNS to `:53`) |
| `smb` | `0.0.0.0:8080` | `0.0.0.0:53` | small business, stricter guard thresholds |

A profile only sets defaults; any explicit `COGWHEEL_*` variable wins over it.
Full variable table: [DEPLOYMENT.md §9](../DEPLOYMENT.md#9-configuration-reference).

## Local run without Docker

```sh
COGWHEEL_PROFILE=dev cargo run -p cogwheel-server
curl -s http://127.0.0.1:30080/health/live
dig @127.0.0.1 -p 30053 example.com +short
```

## Health and metrics endpoints

| Endpoint | Meaning |
|---|---|
| `GET /health/live` | liveness. What the container `HEALTHCHECK` probes. |
| `GET /health/ready` | readiness. Returns **503 until storage, policy and the DNS listeners are all up**, and names which subsystem is lagging. Gate rolling upgrades on this, not on liveness. |
| `GET /metrics` | Prometheus text. Today this is only `cogwheel_startups_total`. |
| `GET /api/v1/runtime` | the numbers that actually matter: cache hits, upstream failures, fallbacks, mean latencies. |

## Day-2 operations

```sh
# Where should clients point?
curl -s http://127.0.0.1:8080/api/v1/resolver-access

# Runtime health and false-positive budget before a change.
curl -s http://127.0.0.1:8080/api/v1/runtime/health
curl -s http://127.0.0.1:8080/api/v1/false-positive-budget

# Logs
docker logs -f cogwheel          # container install
journalctl -u cogwheel -f        # native install
```

- Back up the data directory before any upgrade —
  [DEPLOYMENT.md §11](../DEPLOYMENT.md#11-backup-and-restore). The
  `/api/v1/backup` endpoint is a partial config export, not a full backup.
- Use the load-test and resilience-drill endpoints for soak and failure
  validation.
- Roll back a bad ruleset with `POST /api/v1/rulesets/rollback`.

## Optional: filter Tailscale exit-node DNS

If the node advertises itself as a Tailscale exit node and you want tailnet
traffic filtered too, install the host redirect rule:

```sh
sudo DNS_HOST_PORT=53 scripts/apply-tailscale-dns-intercept.sh
```

Authenticate the node with `tailscale up --advertise-exit-node --accept-dns=false`
so exit-node traffic keeps flowing through Cogwheel.

## Before shipping a change

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo audit
cargo deny check

npm --prefix apps/cogwheel-web ci
npm --prefix apps/cogwheel-web run lint
npm --prefix apps/cogwheel-web run build

shellcheck scripts/*.sh
docker buildx build --check .
```

CI runs all of these on every push.
