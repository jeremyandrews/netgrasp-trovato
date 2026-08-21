# Build the netgrasp overlay, in Docker, for the self-standing demo.
#
# WHY THIS EXISTS
# The overlay is two directories of source (templates/, static/) and one
# directory that has to be assembled: the wasm is a build artifact while the
# manifest and the migrations beside it are source. Assembling it needs a Rust
# toolchain and the wasm32-wasip1 target, which is exactly what the demo promises
# a reader they will not have to install. So the toolchain lives here instead,
# and the only thing the host needs is Docker.
#
# The assembly itself is NOT reimplemented here. scripts/build-overlay.sh is the
# one place that decides the layout `trovato plugin install` expects, and it is
# also what runs scripts/check-host-imports.sh against the artifact it has just
# built. A Dockerfile that ran cargo and cp itself would silently drop that
# check.
#
# The result is a tiny image holding nothing but the assembled plugin directory,
# which docker-compose.demo.yml copies into a named volume and mounts read-only
# into the released kernel.

FROM rust:1-bookworm AS build

WORKDIR /src

# rust-toolchain.toml pins the channel and lists wasm32-wasip1 as a target, so
# rustup provisions both on the first cargo invocation. Nothing here has to name
# a version: the pin is the repository's, not the image's.
COPY . .

RUN scripts/build-overlay.sh

# ---- The artifact, and nothing else ----
FROM busybox:1.37

# One directory: netgrasp.wasm, netgrasp.info.toml, migrations/. The layout the
# kernel's plugin discovery reads.
COPY --from=build /src/overlay/plugins/netgrasp /overlay/netgrasp
