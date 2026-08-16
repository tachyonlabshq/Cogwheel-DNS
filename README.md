# Cogwheel

Network-wide ad and tracker blocking for your home, in one command.

Cogwheel is a DNS filtering appliance written in Rust. Point your router at it and every device on
the network — phones, TVs, consoles, anything that cannot run an ad blocker — stops loading ads and
trackers. It is built for a Raspberry Pi 5, and runs on any 64-bit Linux box.

## Install

On the machine that will run it:

```sh
curl -fsSL https://raw.githubusercontent.com/thekozugroup/Cogwheel-DNS/main/scripts/install.sh | sudo sh
```

That is the whole install. It pulls the image, creates the data directory, **detects and fixes the
port 53 conflict** that trips up most self-hosted DNS servers, waits until the container reports
healthy, and prints two things:

```
Cogwheel is running.

  Web interface   http://192.0.2.10:8080
  DNS address     192.0.2.10        <- put this in your router's DNS setting
```

Open the web interface, follow the router instructions on the Overview screen, and you are done.

Changed your mind? `sudo sh install.sh --uninstall` removes it and puts your host DNS back exactly
as it was.

**Requirements:** 64-bit Linux (`x86_64` or `aarch64`), Docker 24+, and root — binding port 53 and
editing resolver config both need it. On a Raspberry Pi, use the 64-bit OS.

Prefer Docker Compose, or no Docker at all? Both are covered in
[DEPLOYMENT.md](./DEPLOYMENT.md), along with troubleshooting, upgrades and backups.

## How it works

A DNS resolver sits between your devices and the internet. When a device asks for a domain on an
active blocklist, Cogwheel answers immediately with a null address, so the tracker is never
contacted. Everything else is forwarded upstream and cached.

Blocklist updates are staged and verified before they are promoted. If a new ruleset degrades
runtime health, the control plane rolls back to the last known-good policy. A bad upstream list
cannot take your household's DNS down.

Per-device profiles let a child's tablet get strict filtering while a work laptop keeps developer
tools reachable.

## The classifier

Blocklists only cover domains somebody has already reported. Cogwheel also ships a trained
classifier that scores domains blocklists have not seen yet.

It is a calibrated linear model over hashed character n-grams of the hostname, trained on 2.57
million labelled domains drawn from public ad/tracker blocklists and popular-domain rankings, split
by registrable domain so no eTLD+1 appears in more than one split.

Measured on a held-out test set of 245,000 domains:

| Sensitivity | Catches | Wrongly flags |
|---|---|---|
| Low | 17.6% of ad domains | 0.099% of legitimate ones |
| Balanced *(default)* | 33.7% | 0.539% |
| High | 50.2% | 2.317% |

ROC-AUC 0.891. Those are the real numbers, and the UI shows them rather than a marketing claim —
a false positive is a website that stopped working, so you should be able to see the trade you are
making. Thresholds are calibrated to a target false-positive rate rather than to round numbers.

It is a supplement to blocklists, not a replacement for them.

Three things make it safe to run on an appliance:

- **It never touches the DNS hot path.** A query does a 38 ns lookup against verdicts already
  computed; scoring happens on a background thread. A full queue drops work rather than ever making
  a DNS answer wait.
- **A protected-domain list overrides it.** Banking, OS updates, certificate validation, NTP and
  captive-portal checks can never be blocked by the model, whatever it scores them.
- **Every verdict is explainable.** For a linear model the contribution of a feature *is* `w·x`, so
  "why was this blocked?" is answered with real arithmetic, not a templated sentence.

It starts in **Monitor** mode: it scores and reports, and blocks nothing until you say so.

### Correcting it

If it gets one wrong, say so in the UI. Cogwheel stores your reports and, when you ask it to,
trains a small correction from them — on the device, with nothing leaving your network.

The correction is only kept if it passes a check first. Cogwheel re-measures the corrected model
against 25,000 held-out domains and **refuses to apply it** if accuracy drops or false positives
rise at any sensitivity. A rejection is a good outcome, and the UI says which check failed. The
shipped model is never modified, so reverting is one click.

There is also a hard limit on how far a correction can move any score, derived from the model's
own structure rather than from testing. A domain the model is confident about cannot be flipped by
feedback — verified against real domains including `chase.com`, `apple.com` and `letsencrypt.org`.

Scoring is asynchronous, so the first query for a brand-new domain resolves before a verdict
exists; enforcement begins on the next one. The UI says so plainly.

The model ships in the binary as a 1 MiB int8 file. Measured throughput is 140,000 domains/sec/core
on x86 (p99 48.5 µs); a Pi 5 is several times slower per core and still far beyond what a household
generates.

## Stack

- **Rust** — Axum, Hickory DNS, Moka cache, SQLite via rusqlite. No ML runtime dependency: the
  classifier is plain Rust, so an `aarch64` build is just a cross-build.
- **React 19** — Vite, TypeScript, [Shark UI](https://shark.vini.one/), Tailwind CSS v4, self-hosted
  Inter. No CDN requests, because the appliance may sit on a LAN with no internet route.
- **Docker** — multi-arch images for `linux/amd64` and `linux/arm64`.
- **Prometheus** — metrics endpoint.

## Development

```sh
cargo test --workspace                     # 145 tests
cargo clippy --workspace --all-targets --all-features -- -D warnings
cd apps/cogwheel-web && npm ci && npm run build
```

Retraining the classifier is reproducible from public sources:

```sh
node crates/cogwheel-classifier/tools/build-corpus.mjs --out /tmp/corpus --fetch --holdout 25000
cargo run --release -p cogwheel-classifier --features training --bin cogwheel-train -- \
    --corpus /tmp/corpus --out crates/cogwheel-classifier/model/cogwheel-ads-v1.cwm
```

Design and architecture notes live in [docs/architecture/](./docs/architecture/).
