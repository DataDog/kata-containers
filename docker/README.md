# Datadog Kata OCI artifacts

`Dockerfile` packages the CI-built kernel, shim, and guest rootfs. The upstream
Kata release supplies QEMU, virtiofsd, firmware, and configuration templates.
It does not supply the guest OS: that must contain the fork's kata-agent,
Datadog packages, AppArmor profiles, and `datadog-files` overlay.

## Guest rootfs build

The `build-rootfs-amd64` and `build-rootfs-arm64` GitLab jobs export
`kata-rootfs-${arch}.img` and its SBOM with the `docker buildx` local exporter.
Both the Go and Rust OCI jobs consume the same per-architecture guest. The rootfs
uses the existing osbuilder scripts and Ubuntu 22.04 (Jammy).
The build checks the guest agent, system-probe binary and launcher, AppArmor
parser and profiles, agent confinement configuration, and enabled guest services
before exporting the image. These checks verify the artifact contents; boot
validation must also check that the services and confinement are active.

These jobs need **`docker-in-docker:amd64` / `docker-in-docker:arm64`** runners.
The runner supplies the Docker service and `DOCKER_HOST`. Each job creates its
own `docker-container` builder and removes it afterwards. Native runners
avoid emulation during package installation and agent compilation.

The usual compute-delivery image builds use shared, rootless Kubernetes
BuildKit workers. Those workers cannot perform osbuilder's mounts and loop-device
operations. The rootfs builder therefore enables `security.insecure` on its own
daemon and build request; only the osbuilder `RUN` uses that entitlement. A
`devtmpfs` mount inside that step makes loop partitions visible to the existing
image builder. This preserves its ext4 partition and DAX header format.

Runner setup was checked against compute-delivery's v3 `.build-docker-image`
template and dd-source's `domains/devex/ci/gitlab/config/k8s/gitlab-runner/`
configuration. `docker-in-docker` pools disable the shared `docker buildx` setup
and inject the Docker service. Do not change these jobs to plain `arch:*` tags.

For a native local build with Docker and registry access, from the repository root:

```sh
arch=amd64 # use arm64 on an ARM host
builder=kata-rootfs-local
docker buildx create --name "$builder" --driver docker-container \
  --buildkitd-flags '--allow-insecure-entitlement security.insecure'
docker buildx build --builder "$builder" --platform "linux/$arch" \
  --allow security.insecure --ulimit nofile=262144:262144 \
  --file docker/rootfs.Dockerfile \
  --build-arg "SOURCE_COMMIT=$(git rev-parse HEAD)" \
  --output type=local,dest=. .
docker buildx rm "$builder"
```

`rootfs.Dockerfile.dockerignore` deliberately includes `tests/`: the existing
Rust and libseccomp installers source `tests/common.bash`. The repository's
ordinary `.dockerignore` excludes that directory and cannot be used here.

## KPMI migration to OCI

GitLab builds and publishes the OCI image used by NodeInstaller. It includes
`/opt/kata/share/kata-containers/sbom.cdx.gz` so KPMI can preserve the guest SBOM
when it switches to installing `/opt/kata` from the same multi-platform Go OCI
image. The earlier `4.2.0-dd.202638.1` OCI image does not contain this SBOM.

Keep `.github/workflows/build-kata-os.yml` publishing the existing GitHub release
bundles until KPMI has switched to OCI. The migration order is:

1. Merge the OCI SBOM addition and publish a new release; retain the GitHub
   publisher so existing KPMI builds can still use release bundles.
2. Pin the new release and its OCI index digest in the KPMI consumer and
   NodeInstaller chart. Validate the KPMI build and staging installation, then
   merge the consumer changes.
3. Remove the GitHub guest/release publisher in the cleanup follow-up only
   after confirming active consumers no longer need newly published bundles.
   Existing release assets must remain available for older pinned builds.

The OCI consumer work is DataDog/k8s-platform-machine-images#2215 and
DataDog/k8s-platform-resources#27583. Producer #105 prepares the image. Cleanup
#106 only removes unrelated unusable upstream workflows; retiring the GitHub
guest/release publisher needs a separate follow-up after consumer migration.
The Ubuntu 24.04 guest upgrade is independent of that retirement.
