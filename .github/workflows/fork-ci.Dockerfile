# herdr-fork: fork-only. The Linux build and test image shared by fork-ci.yml and
# scripts/fork/linux-test.sh. It mirrors upstream ci.yml's Linux job: ubuntu 24.04
# (ubuntu-latest), Rust 1.96.1 with rustfmt and clippy, Zig 0.16.0, just, cargo-nextest
# and Bun 1.3.14. Bump versions here only. Unofficial stopgap; see FORK.md.
# ubuntu:24.04 as of 2026-10-28, pinned by digest. Bump on purpose.
FROM ubuntu:24.04@sha256:24737cef17f4e22b1977df7d9a05e77c4e28b7da5b8911f8d8ccda136bf3573c
ARG TARGETARCH
ARG RUST_VERSION=1.96.1
ARG RUSTUP_VERSION=1.28.2
ARG ZIG_VERSION=0.16.0
ARG JUST_VERSION=1.58.0
ARG NEXTEST_VERSION=0.9.148
ARG BUN_VERSION=1.3.14
ENV DEBIAN_FRONTEND=noninteractive \
    RUSTUP_HOME=/opt/rustup \
    CARGO_HOME=/opt/cargo \
    PATH=/opt/cargo/bin:/opt/zig:/opt/bun:$PATH
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl xz-utils unzip git build-essential pkg-config python3 bsdutils openssh-client \
 && rm -rf /var/lib/apt/lists/*
RUN set -eu; \
    case "$TARGETARCH" in \
      amd64) triple=x86_64-unknown-linux-gnu; rsha=20a06e644b0d9bd2fbdbfd52d42540bdde820ea7df86e92e533c073da0cdd43c; \
             zarch=x86_64; zsha=70e49664a74374b48b51e6f3fdfbf437f6395d42509050588bd49abe52ba3d00; \
             barch=x64; bsha=951ee2aee855f08595aeec6225226a298d3fea83a3dcd6465c09cbccdf7e848f ;; \
      arm64) triple=aarch64-unknown-linux-gnu; rsha=e3853c5a252fca15252d07cb23a1bdd9377a8c6f3efa01531109281ae47f841c; \
             zarch=aarch64; zsha=ea4b09bfb22ec6f6c6ceac57ab63efb6b46e17ab08d21f69f3a48b38e1534f17; \
             barch=aarch64; bsha=a27ffb63a8310375836e0d6f668ae17fa8d8d18b88c37c821c65331973a19a3b ;; \
      *) echo "unsupported arch $TARGETARCH" >&2; exit 1 ;; \
    esac; \
    curl -fsSLo /tmp/rustup-init "https://static.rust-lang.org/rustup/archive/${RUSTUP_VERSION}/${triple}/rustup-init"; \
    echo "$rsha  /tmp/rustup-init" | sha256sum -c -; \
    chmod +x /tmp/rustup-init; \
    /tmp/rustup-init -y --no-modify-path --profile minimal --default-toolchain "$RUST_VERSION" -c rustfmt -c clippy; \
    rm /tmp/rustup-init; \
    curl -fsSLo /tmp/zig.tar.xz "https://ziglang.org/download/${ZIG_VERSION}/zig-${zarch}-linux-${ZIG_VERSION}.tar.xz"; \
    echo "$zsha  /tmp/zig.tar.xz" | sha256sum -c -; \
    mkdir -p /opt/zig; tar -xJf /tmp/zig.tar.xz -C /opt/zig --strip-components=1; rm /tmp/zig.tar.xz; \
    curl -fsSLo /tmp/bun.zip "https://github.com/oven-sh/bun/releases/download/bun-v${BUN_VERSION}/bun-linux-${barch}.zip"; \
    echo "$bsha  /tmp/bun.zip" | sha256sum -c -; \
    unzip -q /tmp/bun.zip -d /tmp/bun; mkdir -p /opt/bun; mv /tmp/bun/*/bun /opt/bun/bun; rm -rf /tmp/bun /tmp/bun.zip; \
    cargo install --locked "just@${JUST_VERSION}" "cargo-nextest@${NEXTEST_VERSION}"; \
    rm -rf /opt/cargo/registry /opt/cargo/git; \
    chmod -R a+rX /opt/rustup /opt/cargo /opt/zig /opt/bun
# Runs as the caller's uid; HOME and the cargo, target and zig caches are mounted volumes.
ENV HOME=/tmp SHELL=/bin/sh
WORKDIR /src
