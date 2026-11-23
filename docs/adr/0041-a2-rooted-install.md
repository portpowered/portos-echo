---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 23
status: proposed
---

# A2 rooted Athena installation

The native installer on Windows and Linux offers `install --install-path a2` for an already rooted Athena.
The default on an unowned or fork-managed device remains the legacy bundle through coordinator A2 T5.
An active A2 receipt resumes through the A2 owner. Conflicting ownership stops setup before either installer
runs.

The host verifies the release and downloads the Magisk policy tool separately. It pins the APK over HTTPS,
extracts the bounded ARM32 deflate member, and verifies its digest. The existing archive adapter stages
unchanged release bundles. The device install command verifies the staged release again and publishes
`bin/next` under the installer lock. It retains the fork hook and policy preimages during takeover. The
release's A2 boot role owns screen installation and starts the core and local Wyoming device roles.

Native setup retains the selected adb device over reboot. Before account work it observes a different
boot ID and the verified core release running as nobody. A2's private transport uses the boot role's supplied
core credentials and park acknowledgement for account and credential operations. The existing prompt and
forward owners handle ChatGPT, Port OS and API-key mode. A2 wake confirmation uses a fresh run from the local
satellite and the existing paired setup observations.

## Deferred

- B2 and C1 own tool parity and the complete quickstart doctor verdict.
- The coordinator's A2 T5 owns physical USB, reboot, SELinux, Wi-Fi and acoustic wake on a rooted Echo.
- Fork retirement follows A2 T5. The legacy installer stays available during the rollout.
