---
name: porting-kernel-patches
description: Port Datadog guest kernel patches to a new kernel version. Use when you bump KERNEL_VERSION in .gitlab-ci.yml, upgrade the Kata guest kernel to a new major or minor version, or add, remove, or edit files under tools/packaging/kernel/patches/. Covers the required-patch manifest, the check-datadog-patches.sh CI check, how to decide if upstream already fixes a bug under our conditions, and how to verify a port.
---

# Porting Datadog kernel patches

build-kernel.sh applies only the patches in `tools/packaging/kernel/patches/<major>.<minor>.x/`.
A patch in the old version directory does not carry over to a new version.
If you bump the kernel and do not port a patch, the fix disappears without an error.
This happened in #140: the 6.12 bump dropped the FUSE ESTALE fix from #131, and no check failed.

## When to use this skill

- You change `KERNEL_VERSION` in `.gitlab-ci.yml` to a new `<major>.<minor>`.
- You change the guest kernel version in `versions.yaml`.
- You add, remove, or edit a patch under `tools/packaging/kernel/patches/`.

A patch-level bump inside the same series (for example v6.12.100 to v6.12.115) uses the same directory.
For that bump, run the verify steps only.

## Key files

- `tools/packaging/kernel/patches/datadog-required-patches.txt`: Datadog patches that every kernel version must carry.
- `tools/packaging/kernel/check-datadog-patches.sh`: the CI check. It fails if the version directory lacks a required patch.
- `tools/packaging/kernel/build-kernel.sh`: the build script. `setup` applies the patches.
- `.gitlab-ci.yml`: sets `KERNEL_VERSION`. The `check-kernel-patches` job runs the check.

## 1. List the patches to port

Set the old and new directories:

```sh
cd tools/packaging/kernel
OLD=6.8.x   # directory of the current kernel
NEW=6.12.x  # directory of the target kernel
```

List the required Datadog patches:

```sh
grep -v '^#' patches/datadog-required-patches.txt | sed '/^$/d'
```

List every patch in the old directory:

```sh
ls patches/${OLD}/*.patch
```

Run the check against the target version to see what is missing:

```sh
./check-datadog-patches.sh v6.12.115
```

Treat each patch in the old directory as a candidate, not only the patches in the manifest.
Find the origin of each patch with `git log --follow -- patches/${OLD}/<file>`.
If a Datadog PR added the patch, it is a Datadog patch.
If it is a Datadog patch and the manifest does not list it, add it to the manifest (see step 5).

## 2. Check if upstream already fixes the bug

Do this for each patch before you port it.

1. Read the patch and its commit message. Write down the bug and the trigger.
2. Search the target kernel for an upstream fix. Use `git log` on the touched files and search for the subject line.
3. If you find an upstream fix, find the condition under which the upstream code runs.
4. Compare that condition with our setup: the VMM, virtiofsd version and flags, guest kernel config, and workload.
5. Keep the Datadog patch if the upstream code does not run in our setup.

Compare the condition under which the code runs, not only if similar code exists.

Example from the 6.12 bump.
Upstream 6.12 has commit e022f6a1c711 ("fuse: add support for explicit export disabling").
That code only takes effect when the FUSE server sends `FUSE_NO_EXPORT_SUPPORT`.
virtiofsd 1.14 never sends that flag.
Thus the upstream fix does not activate for us, and the Datadog patch is still necessary.

If upstream fully fixes the bug in our setup, do not port the patch.
Remove its manifest entry in the same PR.
Put the evidence in the PR description: the upstream commit, and why its condition is true in our setup.

## 3. Get the target kernel source

Use the exact tag that `KERNEL_VERSION` names.

```sh
git clone --depth 1 --branch v6.12.115 \
  https://git.kernel.org/pub/scm/linux/kernel/git/stable/linux.git /tmp/linux-v6.12.115
```

You can also run `./build-kernel.sh -v v6.12.115 setup` once and use the `kata-linux-*` source tree that it creates.
That tree already has the patches from the new directory applied.

## 4. Port each patch

1. Test the patch on the clean target tree:

   ```sh
   cd /tmp/linux-v6.12.115
   patch -p1 --dry-run < <repo>/tools/packaging/kernel/patches/${OLD}/<file>
   git apply --check <repo>/tools/packaging/kernel/patches/${OLD}/<file>
   ```

2. If both commands succeed, copy the file to `patches/${NEW}/` unchanged.
3. If a hunk fails, apply the patch with `patch -p1` and fix each `.rej` hunk by hand.
   Read the surrounding code in the target tree. Code can move or change names between versions.
4. Commit the result in the kernel tree and export it with `git format-patch -1`.
5. Keep the original subject, body, and `Signed-off-by` lines.
6. Add a line to the commit message body: `Ported to v6.12.115 from v6.8.12.`
   Add a short note for each hunk that you changed by hand.
7. Keep the original file name, so the manifest entry still matches.
8. Remove `no_patches.txt` from `patches/${NEW}/` if the directory now has patches.
   Keep it if the directory has no patches. build-kernel.sh fails if the directory does not exist.

## 5. Update the manifest

Add a new Datadog patch to `datadog-required-patches.txt` in the same PR that adds the patch.

- Write one patch basename per line.
- Put a comment above the entry.
  The comment says what bug the patch fixes, and links the PR.
- Do not add upstream Kata patches. The manifest is for Datadog patches only.

Remove an entry only when upstream fixes the bug in our setup (see step 2).

## 6. Verify

1. Run the regression check:

   ```sh
   cd tools/packaging/kernel
   ./check-datadog-patches.sh v6.12.115
   ```

2. Apply the patches with the real build script. This step must finish without a patch error:

   ```sh
   ./build-kernel.sh -v v6.12.115 setup
   ```

3. Build the kernel if your machine can do it: `./build-kernel.sh -v v6.12.115 build`.
   CI builds amd64 and arm64 in any case.
4. Run the functional test for each patch that has one.

Functional test for `0001-fuse-disable-export-ops-without-export-support.patch`:

1. Start a Kata sandbox with a ZFS rootfs-upper volume.
2. Change `/etc/profile` in the sandbox.
3. Take a ZFS snapshot of the rootfs-upper volume.
4. Restore the snapshot on a new VM.
5. In the new sandbox, run `cat /etc/profile` and `sha256sum /etc/profile`.
6. Both commands must succeed. An `ESTALE` ("Stale file handle") error means that the fix is missing.

## PR checklist for kernel bumps

- [ ] `KERNEL_VERSION` and the target patch directory agree on `<major>.<minor>`.
- [ ] Each patch in the old directory is ported, or the PR explains why it is not.
- [ ] For each patch that you drop, the PR names the upstream commit and why it activates in our setup.
- [ ] Ported patches keep the original message and have a "Ported to X from Y" line.
- [ ] `datadog-required-patches.txt` lists each Datadog patch with a comment and a link.
- [ ] `./check-datadog-patches.sh <version>` passes.
- [ ] `./build-kernel.sh -v <version> setup` applies all patches without errors.
- [ ] The functional test passes for each patch that has one.
- [ ] The CI jobs `check-kernel-patches`, `build-kernel-amd64`, and `build-kernel-arm64` pass.
