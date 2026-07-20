FROM quay.io/pypa/manylinux2014_x86_64

ARG RUST_VERSION

ENV RUSTUP_DIST_SERVER="https://rsproxy.cn" \
    RUSTUP_UPDATE_ROOT="https://rsproxy.cn/rustup"

RUN curl --proto "=https" --tlsv1.2 -sSf https://rsproxy.cn/rustup-init.sh \
    | sh -s -- -y --profile minimal --default-toolchain "${RUST_VERSION}"

ENV PATH="/root/.cargo/bin:${PATH}"
WORKDIR /workspace
