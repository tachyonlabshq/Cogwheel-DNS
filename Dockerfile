# syntax=docker/dockerfile:1.19
#
# Cogwheel DNS — production container image.
#
# Design goals, in priority order:
#   1. Editing Rust source must NOT rebuild third-party crates. That is what
#      the cargo-chef `planner` + `cook` stage pair below buys us.
#   2. The image must build for linux/amd64 and linux/arm64 (Raspberry Pi 5).
#   3. The runtime must be non-root, minimal, and able to bind :53.
#
# Build (single arch, local):
#   docker build -t cogwheel:dev .
#
# Build (both arches, requires a buildx builder with the docker-container driver):
#   docker buildx build --platform linux/amd64,linux/arm64 -t <ref> --push .
#
# BuildKit is required (default since Docker 23) for the `--mount=type=cache`
# and `$BUILDPLATFORM` features used below.

# --------------------------------------------------------------------------
# Pinned inputs.
#
# Every base image is pinned to a MINOR version, not a floating major and not
# `latest`. A rebuild six months from now resolves the same toolchain and the
# same glibc. Bumping these is a deliberate, reviewable commit.
#
# The builder and the runtime share the same Debian suite on purpose: the
# binary is dynamically linked against glibc, so a bookworm-built binary on a
# trixie runtime (or vice versa) is a latent, arch-dependent breakage.
# --------------------------------------------------------------------------
ARG RUST_VERSION=1.94
ARG NODE_VERSION=22.22
ARG DEBIAN_SUITE=bookworm
ARG CARGO_CHEF_VERSION=0.1.78

# ==========================================================================
# Stage: web-builder — build the React/Vite control plane
#
# Pinned to $BUILDPLATFORM deliberately. Vite emits plain static JS/CSS/HTML
# that is byte-identical on every CPU architecture, so there is no reason to
# run npm under QEMU when producing the arm64 image. On an amd64 host building
# for arm64 this turns a multi-minute emulated npm install into a native one.
# ==========================================================================
FROM --platform=$BUILDPLATFORM node:${NODE_VERSION}-${DEBIAN_SUITE}-slim AS web-builder

WORKDIR /build/apps/cogwheel-web

# Manifests first, on their own layer: `npm ci` is only re-run when a
# dependency actually changes, not on every source edit.
COPY apps/cogwheel-web/package.json apps/cogwheel-web/package-lock.json ./
RUN --mount=type=cache,target=/root/.npm,sharing=locked \
    npm ci --no-audit --no-fund

COPY apps/cogwheel-web/ ./
# `npm run build` is `tsc --noEmit -p tsconfig.app.json && vite build`, so a
# type error fails the image build rather than shipping broken assets.
RUN npm run build

# ==========================================================================
# Stage: chef — Rust toolchain plus cargo-chef
#
# cargo-chef is installed from crates.io into the official `rust` image rather
# than using the third-party `lukemathwalker/cargo-chef` base image. That keeps
# every FROM in this file on a Docker Official Image, which is one less
# supply-chain root to trust and one less tag to keep pinned.
#
# The full (non-slim) `rust` image is required: `rusqlite` is built with the
# `bundled` feature, which compiles SQLite's C amalgamation and therefore needs
# a working cc toolchain at build time.
# ==========================================================================
FROM rust:${RUST_VERSION}-${DEBIAN_SUITE} AS chef
ARG CARGO_CHEF_VERSION
WORKDIR /build
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    cargo install cargo-chef --locked --version ${CARGO_CHEF_VERSION}

# ==========================================================================
# Stage: planner — derive the dependency-only build recipe
#
# `cargo chef prepare` reads the workspace manifests and emits recipe.json.
# The whole workspace is copied in because cargo needs a coherent tree to
# resolve, but that is fine: recipe.json only changes when a *dependency*
# changes. BuildKit keys the next stage on the CONTENT of the copied
# recipe.json, so re-running this stage after an ordinary source edit still
# produces identical bytes and the expensive `cook` layer stays cached.
# ==========================================================================
FROM chef AS planner
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
COPY apps/ apps/
RUN cargo chef prepare --recipe-path recipe.json

# ==========================================================================
# Stage: cook — compile ONLY third-party dependencies
#
# This is the layer that makes iteration cheap. It depends on nothing but
# recipe.json, so editing anything under crates/ or apps/ leaves it untouched.
#
# Note what is NOT cache-mounted: /build/target. The compiled dependency
# artifacts are deliberately baked into this image layer instead. BuildKit
# cache mounts are local to a builder and are not exported by `--cache-to`
# (GHA, registry, or otherwise), so a CI runner would find them empty every
# run. Baking them into the layer means `--cache-from type=gha` restores the
# entire cooked dependency tree across CI runs, which is the whole point.
# The cargo registry IS cache-mounted, because that only accelerates the
# download step and re-downloading on a cold CI runner is cheap.
# ==========================================================================
FROM chef AS cook
COPY --from=planner /build/recipe.json recipe.json
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    cargo chef cook --release --recipe-path recipe.json

# ==========================================================================
# Stage: builder — compile the Cogwheel workspace itself
#
# apps/cogwheel-web is intentionally NOT copied here. Rust never reads it, and
# leaving it out means a front-end change cannot invalidate the Rust build.
# ==========================================================================
FROM cook AS builder
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
COPY apps/cogwheel-server/ apps/cogwheel-server/
COPY apps/cogwheel-desktop/ apps/cogwheel-desktop/

# --locked: fail loudly if Cargo.lock does not satisfy the manifests, rather
# than silently resolving different dependency versions than CI tested.
# The release profile already sets strip = true, so no separate strip step.
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    cargo build --release --locked -p cogwheel-server \
 && install -Dm0755 target/release/cogwheel-server /out/cogwheel-server

# ==========================================================================
# Stage: runtime
#
# Why debian:<suite>-slim and not distroless?
#
#   1. File capabilities. Binding :53 as a non-root user requires
#      `setcap cap_net_bind_service=+ep` on the binary. setcap must run in the
#      final stage — `COPY --from` does not reliably carry security.capability
#      extended attributes between stages — and that needs libcap2-bin, which
#      needs a package manager. Distroless has neither.
#   2. HEALTHCHECK. Docker health checks exec a command *inside* the container.
#      Distroless ships no shell and no HTTP client, so a health check there
#      means building and maintaining a second static probe binary.
#   3. Field debuggability. This is a self-hosted appliance sitting on someone's
#      home network. When DNS breaks at 11pm, `docker exec -it cogwheel sh`
#      plus `curl` is the difference between a five-minute fix and a reinstall.
#
# The cost is roughly 30 MB of base layer over distroless/cc. For an appliance
# image that is a good trade. The hardening that actually matters — non-root
# user, dropped capabilities, read-only root filesystem — is applied here and
# in docker-compose.yml, and none of it depends on the base being distroless.
# ==========================================================================
FROM debian:${DEBIAN_SUITE}-slim AS runtime

# ca-certificates: the blocklist updater fetches sources over HTTPS. reqwest is
#   built with the `rustls-tls` feature, which bundles Mozilla's roots via
#   webpki-roots, so Rust itself does not read /etc/ssl — but curl (below) does,
#   and an operator debugging with curl inside the container needs a real trust
#   store. Belt and braces, ~200 KB.
# curl: the HEALTHCHECK probe. Nothing else in the image uses it.
# iproute2: not decoration. GET /api/v1/resolver-access shells out to
#   `ip -6 -o addr show scope global` to work out which addresses to tell the
#   user to point their router at. Without `ip` on PATH that call fails
#   silently and the dashboard simply omits every IPv6 target — and a client
#   that keeps an IPv6 resolver configured bypasses an IPv4-only DNS setting
#   entirely. A missing 2 MB package would present as a filtering bug.
# libcap2-bin: provides setcap, used below to let the non-root binary bind :53.
#   It is NOT purged after use, because `iproute2` has a hard
#   `Depends: libcap2-bin` in Debian — removing it silently drags `ip` out of
#   the image with it, which is exactly the "IPv6 targets vanish from the
#   dashboard" failure described above. Leaving setcap in place is inert at
#   runtime anyway: it needs CAP_SETFCAP and a writable filesystem, and the
#   container has neither (cap_drop: ALL + read_only: true).
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      ca-certificates \
      curl \
      iproute2 \
      libcap2-bin \
 && rm -rf /var/lib/apt/lists/*

# Fixed, non-root uid/gid. It is pinned to a literal value (not "whatever
# useradd picks") because host bind mounts must be chowned to the same number
# by the installer; a floating uid silently produces an unwritable data dir.
ARG COGWHEEL_UID=10001
ARG COGWHEEL_GID=10001
RUN groupadd --system --gid ${COGWHEEL_GID} cogwheel \
 && useradd --system --uid ${COGWHEEL_UID} --gid ${COGWHEEL_GID} \
            --home-dir /app --no-create-home --shell /usr/sbin/nologin cogwheel

WORKDIR /app

COPY --from=builder /out/cogwheel-server /usr/local/bin/cogwheel-server
COPY --from=web-builder /build/apps/cogwheel-web/dist /app/web

# Grant the binary permission to bind ports below 1024 without being root.
#
# `docker run --cap-add=NET_BIND_SERVICE` alone is NOT sufficient for a
# non-root USER. --cap-add only populates the container's *bounding* set, and
# an unprivileged process gains nothing from the bounding set on its own; a
# file capability is what actually transfers the privilege across execve.
#
# Measured on Linux 6.18.5 / Docker 29.3.1, uid 10001, --cap-drop ALL
# --cap-add NET_BIND_SERVICE, reading /proc/self/status:
#
#   binary WITHOUT setcap -> CapBnd 0x400, CapPrm 0x000, CapEff 0x000  (cannot bind :53)
#   binary WITH    setcap -> CapBnd 0x400, CapPrm 0x400, CapEff 0x400  (can bind :53)
#
# Docker's default bounding set already contains CAP_NET_BIND_SERVICE, so this
# works with no extra runtime flags.
#
# On `--security-opt no-new-privileges`: prctl(2) documents no_new_privs as
# rendering file capabilities non-functional, but on the kernel above the
# capability was still granted (CapEff 0x400, NoNewPrivs 1). Because that
# behaviour is not something to bet a household's DNS on across every kernel,
# docker-compose.yml leaves the flag off by default. The arrangement that needs
# no capability at all is bridge networking with DNS bound to :5353 and
# published as 53:5353 — see docker-compose.yml.
RUN setcap 'cap_net_bind_service=+ep' /usr/local/bin/cogwheel-server

# /app/data is created (and owned) in the image so that a fresh Docker named
# volume mounted here inherits 10001:10001 automatically. No VOLUME instruction
# on purpose: VOLUME would force an anonymous volume on every plain
# `docker run`, which is how appliance data quietly gets orphaned on upgrade.
RUN install -d -o ${COGWHEEL_UID} -g ${COGWHEEL_GID} -m 0750 /app/data \
 && chown -R ${COGWHEEL_UID}:${COGWHEEL_GID} /app/web

# --------------------------------------------------------------------------
# Runtime configuration defaults.
#
# These are the real variable names read by cogwheel-api::load_from_env and
# apps/cogwheel-server/src/main.rs. Override any of them at run time.
# --------------------------------------------------------------------------
ENV COGWHEEL_PROFILE=home \
    COGWHEEL_SERVER__HTTP_BIND_ADDR=0.0.0.0:8080 \
    COGWHEEL_SERVER__DNS_UDP_BIND_ADDR=0.0.0.0:53 \
    COGWHEEL_SERVER__DNS_TCP_BIND_ADDR=0.0.0.0:53 \
    COGWHEEL_SERVER__ADVERTISED_DNS_PORT=53 \
    COGWHEEL_STORAGE__DATABASE_URL=sqlite:///app/data/cogwheel.db \
    COGWHEEL_WEB_DIST_DIR=/app/web

USER ${COGWHEEL_UID}:${COGWHEEL_GID}

EXPOSE 8080/tcp 53/udp 53/tcp

# The HTTP port is derived from the configured bind address rather than being
# duplicated, so overriding COGWHEEL_SERVER__HTTP_BIND_ADDR keeps the health
# check pointed at the right place. ${addr##*:} takes everything after the last
# colon, which is correct for both "0.0.0.0:8080" and "[::]:8080".
#
# /health/live is the liveness probe (see DEPLOYMENT.md). /health/ready exists
# and is documented, but it is currently an unconditional stub, so it is not a
# stronger signal than /health/live for gating startup.
HEALTHCHECK --interval=30s --timeout=5s --start-period=45s --retries=3 \
  CMD ["/bin/sh", "-c", "addr=\"${COGWHEEL_SERVER__HTTP_BIND_ADDR:-0.0.0.0:8080}\"; exec curl -fsS -o /dev/null --max-time 4 \"http://127.0.0.1:${addr##*:}/health/live\""]

# Explicit, even though SIGTERM is the default: the shutdown path matters for
# an appliance and should not be an accident of Docker's defaults.
STOPSIGNAL SIGTERM

ENTRYPOINT ["/usr/local/bin/cogwheel-server"]

# --------------------------------------------------------------------------
# OCI image metadata. VERSION/REVISION/CREATED are supplied by CI
# (see .github/workflows/release.yml); they are last so that a changed build
# argument only invalidates this final metadata layer.
# --------------------------------------------------------------------------
ARG VERSION=0.0.0-dev
ARG REVISION=unknown
ARG CREATED=1970-01-01T00:00:00Z
ARG DEBIAN_SUITE
LABEL org.opencontainers.image.title="Cogwheel DNS" \
      org.opencontainers.image.description="Rust DNS adblock appliance with per-device block profiles" \
      org.opencontainers.image.url="https://github.com/tachyonlabshq/Cogwheel-DNS" \
      org.opencontainers.image.source="https://github.com/tachyonlabshq/Cogwheel-DNS" \
      org.opencontainers.image.documentation="https://github.com/tachyonlabshq/Cogwheel-DNS/blob/main/DEPLOYMENT.md" \
      org.opencontainers.image.vendor="Tachyon Labs" \
      org.opencontainers.image.licenses="MIT" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.revision="${REVISION}" \
      org.opencontainers.image.created="${CREATED}" \
      org.opencontainers.image.base.name="docker.io/library/debian:${DEBIAN_SUITE}-slim"
