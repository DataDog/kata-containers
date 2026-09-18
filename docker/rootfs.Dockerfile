# syntax=docker/dockerfile:1-labs
# Build on the native docker-in-docker runner for TARGETARCH. Only the osbuilder
# RUN needs privileges; the shared rootless CI BuildKit workers cannot run it.
FROM registry.ddbuild.io/images/base/gbi-ubuntu_2204:release AS builder
USER root
SHELL ["/bin/bash", "-o", "pipefail", "-c"]

# Ubuntu rootfs-builder dependencies plus the ext4/DAX image-builder tools.
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        ca-certificates curl g++ git gnupg2 libclang-dev make makedev mmdebstrap \
        musl-dev musl-tools pkg-config protobuf-compiler xz-utils file wget zstd \
        e2fsprogs gdisk parted qemu-utils util-linux xfsprogs && \
    rm -rf /var/lib/apt/lists/*

# The existing mmdebstrap customize hook generates the guest's SBOM with Trivy.
RUN curl -fsSL https://aquasecurity.github.io/trivy-repo/deb/public.key | \
        gpg --dearmor -o /usr/share/keyrings/trivy.gpg && \
    echo 'deb [signed-by=/usr/share/keyrings/trivy.gpg] https://aquasecurity.github.io/trivy-repo/deb generic main' > /etc/apt/sources.list.d/trivy.list && \
    apt-get update && apt-get install -y --no-install-recommends trivy && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /kata-containers
COPY . .
ENV PATH="/root/.cargo/bin:${PATH}" INSTALL_IN_GOPATH=false
# Resolve toolchain versions through the repository's existing installers.
RUN ci/install_yq.sh && target_branch=datadog tests/install_rust.sh && \
    if [ ! -e "/usr/bin/$(uname -m)-linux-musl-gcc" ]; then \
        ln -s /usr/bin/musl-gcc "/usr/bin/$(uname -m)-linux-musl-gcc"; \
    fi

ARG TARGETARCH
ARG SOURCE_COMMIT
# Run the existing Ubuntu guest and disk-image osbuilder scripts
# directly inside BuildKit, without nested docker run or remote bind mounts.
# /rootfs is also the path expected by the existing SBOM hook. Mount devtmpfs
# inside this RUN so loop partitions created by the kernel appear in /dev.
RUN --security=insecure \
    test -n "${SOURCE_COMMIT}" && \
    mount -t devtmpfs devtmpfs /dev && \
    trap 'umount -l /dev' EXIT && \
    export OS_VERSION=jammy INSIDE_CONTAINER=1 USER=root GROUP=root target_branch=datadog \
        MAKEFLAGS="COMMIT_NO=${SOURCE_COMMIT}" && \
    tools/osbuilder/rootfs-builder/rootfs.sh -d -o "$(cat VERSION)-${SOURCE_COMMIT}" -r /rootfs ubuntu && \
    test -x /rootfs/usr/bin/kata-agent && \
    test -x /rootfs/opt/datadog-agent/embedded/bin/system-probe && \
    test -x /rootfs/sbin/apparmor_parser && \
    test -s /rootfs/etc/apparmor.d/kata-container && \
    test -s /rootfs/etc/apparmor.d/usr.bin.kata-agent && \
    test -x /rootfs/usr/local/bin/start-system-probe && \
    test -s /rootfs/etc/systemd/system/kata-agent.service.d/50-apparmor.conf && \
    test "$(chroot /rootfs systemctl is-enabled system-probe.service)" = enabled && \
    test "$(chroot /rootfs systemctl is-enabled datadog-apparmor.service)" = enabled && \
    mkdir /out && \
    tools/osbuilder/image-builder/image_builder.sh -o "/out/kata-rootfs-${TARGETARCH}.img" /rootfs && \
    cp sbom.cdx.gz "/out/kata-rootfs-${TARGETARCH}.sbom.cdx.gz"

FROM scratch AS artifacts
COPY --from=builder /out/ /
