---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 15
status: accepted
---

# Maintained WebView screen client

Following S3, `org.portos.echoscreen` is an ordinary Java Activity hosting the
bridge's existing page. It uses API 28, JavaScript and DOM storage, immersive
display and keep-awake, with raw 16/10 px top/bottom insets. Private preferences
retain the last valid page URL; backup is disabled. No JavaScript bridge,
credential export, page protocol change or third-party dependency is added.

Pure Java policies own URL selection, constant redacted diagnostics and initial
load recovery. Initial main-frame failures show a native waiting surface and
retry at 1, 2, 4, 8, 16, 30, 30... seconds. Success resets backoff and leaves later
transport recovery to the page. Replacing configuration or destroying the
Activity cancels stale work. The server supplies a secret-wrapped URL with
nested same-origin event/action endpoints; the app does not construct it.

Build with preinstalled SDK build-tools 35.0.0, platform android-34 and a JDK,
without Gradle or SDK installation. Compile platform 34 does not raise the
min/target API 28 contract. CI uses an ignored build-directory signing key;
durable installations need an operator keystore outside the repository.

The existing unconditional `build-host` job and matching Make target build,
test and verify the APK, preserving the 15-job CI parity contract. Host policy
and URL tests prove controlled behavior; aapt2/apksigner inspect the actual
package. Installer tests execute the real CLI with the prebuilt fake adb and
exact private argv ledgers. URL shell quoting and fixed error output protect
pairing secrets; no failed step proceeds to a later device command.

The coordinator alone installs on hardware. The installer writes hassmic's
existing one-line home selection and starts the Activity; uninstall removes
the selection and package. Failure may leave partial setup and never promises
rollback of an unknown prior app. Android runtime, boot, physical overlays,
touch, memory and LAN/TLS remain the operator's M1 gate. Browser component
evidence cannot establish those device properties.
