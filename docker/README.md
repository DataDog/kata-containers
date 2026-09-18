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

## KPMI installation

GitLab's native jobs export the guest image and SBOM (`build-rootfs-*`),
kernel and kernel configuration (`build-kernel-*`), and Go shim (`build-shim-*`).
The OCI publisher consumes these outputs. KPMI downloads the same job archives
by numeric job ID and SHA-256 checksum, then installs the files directly.
There is no separate guest build, GitHub release upload, or OCI extraction in
KPMI. QEMU and firmware still come from the upstream Kata version used by OCI.

When promoting a release, select the successful jobs from the tag pipeline
that produced the OCI image. Keep all six referenced archives in GitLab before
pinning their URLs and checksums in KPMI's `config/downloads.yaml`. Use the job
page's **Keep** action or `POST /projects/3675/jobs/<job-id>/artifacts/keep`;
confirm `artifacts_expire_at` is null. Do not use latest-by-branch download URLs.

KPMI downloads on its CI controller with `CI_JOB_TOKEN`; local builds can use
`GITLAB_TOKEN`. Credentials are not forwarded on artifact-storage redirects.
The existing `4.2.0-dd.202638.1` native artifacts are retained and match that
release's OCI payload, so this installation change needs no new image release.
