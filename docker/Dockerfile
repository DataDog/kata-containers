# Builder stage
FROM ubuntu:22.04 AS builder

# Set environment variables for versions
ARG KATA_VERSION=3.18.0
ARG KATA_URL="https://github.com/kata-containers/kata-containers/releases/download/3.18.0/kata-static-3.18.0-amd64.tar.xz"
ARG KATA_CHECKSUM="sha256:ee3dfd665e458a129afb58927143b6bd9acafc4caf4c892312a2bf0124c57a21"

# Install packages and download kata package
RUN apt-get update && \
    apt-get install -y curl wget unzip xz-utils ca-certificates && \
    rm -rf /var/lib/apt/lists/*

# Create directory structure
RUN mkdir -p /kata-build/opt/kata/bin \
             /kata-build/opt/kata/conf/qemu/config.d \
             /kata-build/opt/kata/libexec \
             /kata-build/opt/kata/share/kata-containers

# Download and extract kata package
RUN wget -q "${KATA_URL}" -O /tmp/kata.tar.xz && \
    echo "${KATA_CHECKSUM}  /tmp/kata.tar.xz" | sha256sum -c - && \
    tar -xJf /tmp/kata.tar.xz -C /kata-build \
        './opt/kata/bin/qemu-system-x86_64' \
        './opt/kata/libexec/virtiofsd' \
        './opt/kata/lib/kata-qemu/libfdt.a' \
        './opt/kata/lib/kata-qemu/pkgconfig/libfdt.pc' \
        './opt/kata/include/fdt.h' \
        './opt/kata/include/libfdt.h' \
        './opt/kata/include/libfdt_env.h' \
        './opt/kata/share/kata-qemu/qemu/bios-256k.bin' \
        './opt/kata/share/kata-qemu/qemu/bios-microvm.bin' \
        './opt/kata/share/kata-qemu/qemu/bios.bin' \
        './opt/kata/share/kata-qemu/qemu/edk2-i386-code.fd' \
        './opt/kata/share/kata-qemu/qemu/edk2-i386-secure-code.fd' \
        './opt/kata/share/kata-qemu/qemu/edk2-i386-vars.fd' \
        './opt/kata/share/kata-qemu/qemu/edk2-x86_64-code.fd' \
        './opt/kata/share/kata-qemu/qemu/edk2-x86_64-secure-code.fd' \
        './opt/kata/share/kata-qemu/qemu/edk2-licenses.txt' \
        './opt/kata/share/kata-qemu/qemu/efi-virtio.rom' \
        './opt/kata/share/kata-qemu/qemu/firmware/50-edk2-i386-secure.json' \
        './opt/kata/share/kata-qemu/qemu/firmware/50-edk2-x86_64-secure.json' \
        './opt/kata/share/kata-qemu/qemu/firmware/60-edk2-i386.json' \
        './opt/kata/share/kata-qemu/qemu/firmware/60-edk2-x86_64.json' \
        './opt/kata/share/kata-qemu/qemu/kvmvapic.bin' \
        './opt/kata/share/kata-qemu/qemu/linuxboot.bin' \
        './opt/kata/share/kata-qemu/qemu/linuxboot_dma.bin' \
        './opt/kata/share/kata-qemu/qemu/multiboot_dma.bin' \
        './opt/kata/share/kata-qemu/qemu/pvh.bin' \
        './opt/kata/share/kata-qemu/qemu/qboot.rom' \
        './opt/kata/share/kata-qemu/qemu/qemu-nsis.bmp' \
        './opt/kata/share/defaults/kata-containers/configuration-qemu.toml'

# Copy CI-built artifacts
COPY tools/packaging/kernel/vmlinux-amd64 /kata-build/opt/kata/share/kata-containers/vmlinux-amd64
COPY tools/packaging/kernel/vmlinux-arm64 /kata-build/opt/kata/share/kata-containers/vmlinux-arm64
COPY tools/osbuilder/kata-containers-image-ubuntu.img /kata-build/opt/kata/share/kata-containers/kata-containers.img

# Link configuration
RUN ln /kata-build/opt/kata/share/defaults/kata-containers/configuration-qemu.toml /kata-build/opt/kata/conf/qemu/configuration-qemu.toml

# Copy override configuration
COPY docker/10-override.toml /kata-build/opt/kata/conf/qemu/config.d/10-override.toml

# Final scratch-based data volume
FROM scratch
COPY --from=builder /kata-build /