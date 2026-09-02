# Builder stage
FROM registry.ddbuild.io/images/base/gbi-ubuntu_2404:release AS builder

# GBI images default to a non-root user; switch to root for the apt/install
# steps below. The final image is scratch, so this only affects the builder.
USER root

# The upstream kata-static release to bundle. Defaults to the repo's VERSION
# file so it tracks the fork automatically; override with --build-arg KATA_VERSION.
ARG KATA_VERSION=""

# Install packages
RUN apt-get update && \
    apt-get install -y curl wget unzip xz-utils zstd ca-certificates && \
    rm -rf /var/lib/apt/lists/*

# Create directory structure. conf/{qemu,qemu-tapnet,qemu-jailnet} mirror the
# multiruntime layout k8s-platform-machine-images bakes into the AMI — the shim
# routes the kata-tapnet / kata-jailnet backends to configs under these dirs.
RUN mkdir -p /kata-build/opt/kata/bin \
             /kata-build/opt/kata/conf/qemu/config.d \
             /kata-build/opt/kata/conf/qemu-tapnet/config.d \
             /kata-build/opt/kata/conf/qemu-jailnet/config.d \
             /kata-build/opt/kata/libexec \
             /kata-build/opt/kata/share/kata-containers \
             /kata-build/etc/containerd/config.d

# Download and extract the upstream kata-static package for this arch. The
# version defaults to the repo's VERSION file so it tracks the fork automatically.
ARG TARGETARCH
COPY VERSION /tmp/kata-version
RUN KATA_VERSION="${KATA_VERSION:-$(cat /tmp/kata-version)}" && \
    echo "Downloading kata-static ${KATA_VERSION} (${TARGETARCH})" && \
    wget -q "https://github.com/kata-containers/kata-containers/releases/download/${KATA_VERSION}/kata-static-${KATA_VERSION}-${TARGETARCH}.tar.zst" -O /tmp/kata.tar.zst && \
    tar --zstd -xf /tmp/kata.tar.zst -C /kata-build --wildcards \
        './opt/kata/bin/qemu-system-*' \
        './opt/kata/libexec/virtiofsd' \
        './opt/kata/lib/kata-qemu/*' \
        './opt/kata/include/*' \
        './opt/kata/share/kata-qemu/qemu/*' \
        './opt/kata/share/defaults/kata-containers/configuration-qemu.toml' \
        './opt/kata/share/kata-containers/kata-ubuntu-noble.image' && \
    # kata bundles edk2 UEFI firmware for every arch (~290MB of cross-arch
    # blobs); keep only the target arch's. arm64 uses aarch64, not 32-bit arm.
    case "${TARGETARCH}" in \
        amd64) keep_edk2='edk2-(i386|x86_64)-' ;; \
        arm64) keep_edk2='edk2-aarch64-' ;; \
        *)     keep_edk2='edk2-' ;; \
    esac && \
    find /kata-build/opt/kata/share/kata-qemu/qemu -maxdepth 1 -name 'edk2-*.fd' \
        | grep -Ev "${keep_edk2}" | xargs -r rm -f && \
    mv /kata-build/opt/kata/share/kata-containers/kata-ubuntu-noble.image /kata-build/opt/kata/share/kata-containers/kata-containers-datadog.img && \
    rm -f /tmp/kata.tar.zst

# Copy CI-built kernel for the target architecture and rename to datadog convention
COPY tools/packaging/kernel/vmlinux-${TARGETARCH} /kata-build/opt/kata/share/kata-containers/vmlinux-datadog

# Copy the CI-built kernel config alongside the kernel so it ships in the image
COPY tools/packaging/kernel/kernel-config-${TARGETARCH} /kata-build/opt/kata/share/kata-containers/config-datadog

# Copy CI-built shim binary
# This should be built by the CI pipeline and placed in the expected location
COPY src/runtime/containerd-shim-kata-v2-${TARGETARCH} /kata-build/opt/kata/bin/containerd-shim-kata-v2
RUN chmod +x /kata-build/opt/kata/bin/containerd-shim-kata-v2

# Create symlinks for kata-containers.img and vmlinux.container
RUN ln -s /opt/kata/share/kata-containers/kata-containers-datadog.img /kata-build/opt/kata/share/kata-containers/kata-containers.img && \
    ln -s /opt/kata/share/kata-containers/vmlinux-datadog /kata-build/opt/kata/share/kata-containers/vmlinux.container

# Copy the base qemu config into each runtime-variant conf dir. A real copy (not a
# symlink) is required: kata resolves config.d/ relative to the config file's own
# directory, so the base config must physically live under each conf/<variant>/.
RUN base=/kata-build/opt/kata/share/defaults/kata-containers/configuration-qemu.toml && \
    cp -a "$base" /kata-build/opt/kata/conf/qemu/configuration-qemu.toml && \
    cp -a "$base" /kata-build/opt/kata/conf/qemu-tapnet/configuration-qemu-tapnet.toml && \
    cp -a "$base" /kata-build/opt/kata/conf/qemu-jailnet/configuration-qemu-jailnet.toml

# Copy the per-variant override fragments. The multiruntime shim routes the
# kata-tapnet / kata-jailnet backends here via KATA_CONF_FILE, so they must ship in
# the OCI image — otherwise the /opt/kata bind-mount hides the AMI-baked copies and
# multi-runtime breaks.
COPY docker/containerd/config.d/10-override.toml /kata-build/opt/kata/conf/qemu/config.d/10-override.toml
COPY docker/containerd/config.d/qemu-tapnet/10-override.toml /kata-build/opt/kata/conf/qemu-tapnet/config.d/10-override.toml
COPY docker/containerd/config.d/qemu-jailnet/10-override.toml /kata-build/opt/kata/conf/qemu-jailnet/config.d/10-override.toml

# Copy containerd runtime dropin
COPY docker/containerd/config.d/20-kata-runtime.toml /kata-build/etc/containerd/config.d/20-kata-runtime.toml

# Stamp the image version into /opt/kata so the node-installer validation can
# prove the /opt/kata bind-mounted on the host is THIS OCI image (not the
# AMI-baked copy): the validation compares this file against the tag the kata
# NodeInstaller CR references. CI passes OCI_VERSION as the image tag (the same
# value the image is tagged with). The Dockerfile itself only knows the upstream
# KATA_VERSION, so the DD tag has to come in as a build arg.
ARG OCI_VERSION=""
RUN echo "${OCI_VERSION}" > /kata-build/opt/kata/DATADOG_KATA_VERSION

# Final scratch-based data volume
FROM scratch
COPY --from=builder /kata-build/opt/kata /opt/kata
COPY --from=builder /kata-build/etc/containerd /etc/containerd
