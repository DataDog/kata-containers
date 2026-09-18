#!/usr/bin/env bash
# Publish the GitLab-built KPMI bundles without replacing a published release.
set -euo pipefail

: "${CI_COMMIT_TAG:?release tag is required}"
repo=DataDog/kata-containers
bundle_dir=${1:-release-bundles}
metadata=$(mktemp)
trap 'rm -f "${metadata}"' EXIT

# Verify the tag before creating anything; auth/API errors must fail closed.
gh api "repos/${repo}/git/ref/tags/${CI_COMMIT_TAG}" >/dev/null
gh api --paginate "repos/${repo}/releases?per_page=100" --jq ".[] | select(.tag_name == \"${CI_COMMIT_TAG}\")" > "${metadata}"
if [[ ! -s "${metadata}" ]]; then
    gh release create "${CI_COMMIT_TAG}" --repo "${repo}" --verify-tag --draft --title "${CI_COMMIT_TAG}" \
        --notes 'Datadog guest rootfs, SBOM, kernel and shims built by GitLab CI. The Go and Rust OCI images consume the same build outputs.'
    gh release view "${CI_COMMIT_TAG}" --repo "${repo}" --json isDraft,assets > "${metadata}"
else
    # Normalize the REST shape to the gh release view shape.
    jq '{isDraft: .draft, assets: .assets}' "${metadata}" > "${metadata}.normalized"
    mv "${metadata}.normalized" "${metadata}"
fi

if [[ "$(jq -r .isDraft "${metadata}")" != true ]]; then
    # Retrying a successful publication is safe only when all assets match.
    for asset in "${bundle_dir}"/kata-artifacts-{amd64,arm64}.zip "${bundle_dir}"/kata-checksum-{amd64,arm64}.sha256; do
        expected="sha256:$(sha256sum "${asset}" | cut -d' ' -f1)"
        actual=$(jq -r --arg name "$(basename "${asset}")" '.assets[] | select(.name == $name) | .digest' "${metadata}")
        [[ "${actual}" = "${expected}" ]] || { echo "Published asset differs or is missing: ${asset}" >&2; exit 1; }
    done
    exit 0
fi

gh release upload "${CI_COMMIT_TAG}" --repo "${repo}" --clobber \
    "${bundle_dir}"/kata-artifacts-{amd64,arm64}.zip "${bundle_dir}"/kata-checksum-{amd64,arm64}.sha256
gh release edit "${CI_COMMIT_TAG}" --repo "${repo}" --draft=false
