FROM registry.ddbuild.io/images/base/gbi-ubuntu_2404:release

ARG GOLANG_VERSION=1.22.2
ARG TARGETARCH

USER root

# Install the required packages
RUN apt-get update && apt-get install -y \
    flex \
    bison \
    libelf-dev \
    libssl-dev \
    pahole \
    xz-utils \
    gcc \
    make \
    patch \
    bc \
    ca-certificates \
    curl \
    python3-venv

# Install golang
RUN curl -fs https://dl.google.com/go/go$GOLANG_VERSION.linux-${TARGETARCH}.tar.gz | tar -v -C /usr/local -xz
ENV GOPATH=/go
ENV PATH=$GOPATH/bin:/usr/local/go/bin:$PATH

# Install yq

RUN go install github.com/mikefarah/yq/v4@latest

# Ubuntu 24.04 manages the system Python environment externally. Keep the AWS
# CLI in its own environment rather than bypassing that protection.
RUN python3 -m venv /opt/awscli && \
    /opt/awscli/bin/pip install --no-cache-dir awscli && \
    ln -s /opt/awscli/bin/aws /usr/local/bin/aws
