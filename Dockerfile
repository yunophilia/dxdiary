# Toolchain image for dxdiary. Build-only -- nothing ships from here, so
# there is no runtime stage and no layer-caching machinery.
#
# The image carries the cross toolchains; the source is bind-mounted at run
# time and cargo does its own incremental caching into a named volume. That
# keeps the image static (rebuilt only when the toolchain changes) and makes
# rebuilds as fast as a native cargo build.
#
# Linux only. dxdiary reads raw stdin with poll(2) for terminal capability
# queries, and on Windows the use case is WSL, which is Linux -- so a Windows
# target would mean a parallel console-API implementation for nobody.
#
# Driven by scripts/build.sh.

ARG RUST_VERSION=1.98
FROM rust:${RUST_VERSION}-bookworm

# tree-sitter grammars are C, so every target needs a real cross-compiler --
# a Rust target alone is not enough.
RUN apt-get update && apt-get install -y --no-install-recommends \
        musl-tools \
        gcc-aarch64-linux-gnu \
        pkg-config \
    && rm -rf /var/lib/apt/lists/*

RUN rustup target add \
        x86_64-unknown-linux-gnu \
        x86_64-unknown-linux-musl \
        aarch64-unknown-linux-gnu

# Two separate lookups, both required: the `cc` crate reads CC_<target> to
# build tree-sitter's C; cargo reads CARGO_TARGET_<TARGET>_LINKER to link the
# Rust binary. Missing either produces a confusing failure.
ENV CC_x86_64_unknown_linux_musl=musl-gcc \
    CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=musl-gcc \
    \
    CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc \
    CXX_aarch64_unknown_linux_gnu=aarch64-linux-gnu-g++ \
    AR_aarch64_unknown_linux_gnu=aarch64-linux-gnu-ar \
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc

WORKDIR /app
