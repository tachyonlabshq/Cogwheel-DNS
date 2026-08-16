# User Quick Start

This guide is for someone using Cogwheel as a DNS filtering appliance.

## Open the Dashboard

Visit the Cogwheel web UI in your browser:

- Standard deployment: `http://<your-cogwheel-host>:8080`
- Local development: `http://localhost:30080`

Whoever installed Cogwheel will have the address; the installer prints it at
the end, and it is the same address your router's DNS setting points at.

## Make Sure Your Devices Are Actually Using It

Filtering only applies to devices whose DNS goes through Cogwheel. The reliable
way is to set it once on your router, in the DHCP settings, so every device is
covered — including the ones you cannot configure.

If some sites are filtered and others are not, the most common cause is IPv6:
a device with an IPv6 DNS server configured will ignore an IPv4-only setting.
Ask your operator to set both addresses. See
[DEPLOYMENT.md](../DEPLOYMENT.md#6-pointing-your-router-at-cogwheel).

## First Things to Configure

Start with the small set of user-facing controls:

1. Pick a blocklist preset: `Essential`, `Balanced`, or `Aggressive`.
2. Set classifier mode: `Off`, `Monitor`, or `Protect`.
3. Adjust sensitivity only if needed.

Everything else stays under advanced diagnostics and recovery flows.

## What the Main Dashboard Shows

- Runtime health and protection status
- Recent audit and security activity
- Notification delivery health
- Sync status for multi-node setups
- Tailscale exit-node and DNS filtering status
- False-positive budget readiness

## Safe Recovery Features

Cogwheel includes:

- Backup and restore APIs for recovery
- Rollback-aware Tailscale controls
- Resilience drills to validate operations
- Load-test tools for operator validation

## If Browsing Breaks

Try these steps in order:

1. Switch to a less aggressive blocklist preset.
2. Move classifier mode from `Protect` to `Monitor`.
3. Review recent security or audit events in the dashboard.
4. Ask your operator to restore from backup or roll back a recent change.

## If You Use Tailscale

When exit-node mode is enabled, Cogwheel can filter DNS for tailnet traffic.

Check the Tailscale card for:

- whether Tailscale is installed
- whether the daemon is running
- whether exit-node mode is active
- DNS filtering guidance and rollback controls
