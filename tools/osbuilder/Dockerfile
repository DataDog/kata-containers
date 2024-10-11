FROM registry.ddbuild.io/images/base/gbi-ubuntu_2204:release

USER root

ARG GO_VERSION=1.22.2
ENV GO_HOME="/opt"
ENV GOCACHE="${GO_HOME}/.cache"
ENV RUSTUP_HOME="/opt/rustup"
ENV CARGO_HOME="/opt/cargo"
ENV PATH="/opt/cargo/bin/:/opt/go/bin:${PATH}"

ENV GO_HOME="/opt"
ENV PATH="/opt/go/bin:${PATH}"

SHELL ["/bin/bash", "-o", "pipefail", "-c"]

RUN mkdir ${RUSTUP_HOME} ${CARGO_HOME} ${GOCACHE} && \
    chmod -R a+rwX ${RUSTUP_HOME} ${CARGO_HOME} ${GO_HOME}

# makedev tries to mknod from postinst
RUN [ -x /usr/bin/systemd-detect-virt ] || ( echo "echo docker" >/usr/bin/systemd-detect-virt && chmod +x /usr/bin/systemd-detect-virt )
# hadolint ignore=DL3009,SC2046
RUN apt-get update && \
    DEBIAN_FRONTEND=noninteractive \
    apt-get --no-install-recommends -y install \
    ca-certificates \
    curl \
    g++ \
    git \
    gnupg2 \
    libclang-dev \
    make \
    makedev \
    multistrap \
    musl \
    musl-dev \
    musl-tools \
    protobuf-compiler \
    xz-utils \
    pip \
    python3-dev \
    zstd && \
    apt-get clean && rm -rf /var/lib/apt/lists/&& \
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain 1.76.0

RUN ARCH=$(uname -m); \
    goarch=""; \
    kernelname=$(uname -s | tr '[:upper:]' '[:lower:]'); \
    case "${ARCH}" in \
    "aarch64") goarch="arm64" ;; \
    "ppc64le") goarch=${ARCH} ;; \
    "x86_64") goarch="amd64" ;; \
    "s390x") goarch=${ARCH} ;; \
    *) echo "Unsupported architecture: ${ARCH}" && exit 1 ;; \
    esac; \
    curl -OL "https://storage.googleapis.com/golang/go${GO_VERSION}.${kernelname}-${goarch}.tar.gz" && \
    tar -C "${GO_HOME}" -xzf "go${GO_VERSION}.${kernelname}-${goarch}.tar.gz" && \
    rm "go${GO_VERSION}.${kernelname}-${goarch}.tar.gz"

# aarch64 requires this name -- link for all
RUN if [ ! -f "/usr/bin/$(uname -m)-linux-musl-gcc" ]; then ln -s /usr/bin/musl-gcc "/usr/bin/$(uname -m)-linux-musl-gcc"; fi

RUN ARCH=$(uname -m); \
    rust_arch=""; \
    libc=""; \
    case "${ARCH}" in \
    "aarch64") rust_arch="${ARCH}"; libc="musl"; ;; \
    "ppc64le") rust_arch="powerpc64le"; libc="gnu"; ;; \
    "x86_64") rust_arch="${ARCH}"; libc="musl"; ;; \
    "s390x") rust_arch="${ARCH}"; libc="gnu"; ;; \
    *) echo "Unsupported architecture: ${ARCH}" && exit 1 ;; \
    esac; \
    rustup target add "${rust_arch}-unknown-linux-${libc}"

RUN pip install --no-cache-dir pyinstaller==6.9.0
