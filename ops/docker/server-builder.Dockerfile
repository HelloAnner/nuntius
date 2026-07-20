FROM quay.io/pypa/manylinux2014_x86_64

ARG RUST_VERSION

RUN curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --profile minimal --default-toolchain "${RUST_VERSION}"

ENV PATH="/root/.cargo/bin:${PATH}"
WORKDIR /workspace
