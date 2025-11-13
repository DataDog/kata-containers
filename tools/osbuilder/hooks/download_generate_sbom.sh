#!/bin/bash -eux

trivy fs /rootfs -f cyclonedx -o /kata-containers/sbom.cdx 2>&1 | tee /kata-containers/trivy.log
rm  /kata-containers/sbom.cdx.gz
gzip /kata-containers/sbom.cdx
