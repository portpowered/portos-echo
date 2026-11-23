---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 23
status: accepted for A2 implementation
---

# A2 Wi-Fi device socket

The system-UID device role owns Wi-Fi through Android 9's `IWifiManager`. The Android bridge embeds a dex
built from the reviewed helper sources with the same JDK and Android SDK tools as Echo HOME. It runs
`app_process` with the device identity and carries the request through a private pipe. The pinned fork's
[Wi-Fi helper][fork] supplies the Fire OS 7 Binder method and caller-package references. Its MIT notice
ships with `echo-bridge notices`.

A local sibling stream at `/data/misc/portos-echo-device/wifi.sock` inherits the A1 device directory
and socket ownership (`system:3990`, modes 0750 and 0660 respectively) and uses its own lease. The
device role accepts one LF-terminated JSON request and emits one JSON result. The Wi-Fi worker runs
outside the voice session. Wyoming clients keep their original protocol and can stay connected
throughout Wi-Fi work.

```json
{"type":"wifi-request","data":{"version":1,"op":"status"}}
{"type":"wifi-result","data":{"version":1,"op":"status","ok":true,"ssid":null,"reachable":false}}
{"type":"wifi-request","data":{"version":1,"op":"scan"}}
{"type":"wifi-result","data":{"version":1,"op":"scan","ok":true,"networks":[]}}
{"type":"wifi-request","data":{"version":1,"op":"join","ssid":"Fixture B","password":"<synthetic-input>"}}
{"type":"wifi-result","data":{"version":1,"op":"join","ok":true,"ssid":"Fixture B","reachable":true}}
{"type":"wifi-request","data":{"version":1,"op":"forget","networkId":7}}
{"type":"wifi-result","data":{"version":1,"op":"forget","ok":true,"ssid":null,"reachable":false}}
```

A scan sorts observed networks by descending dBm and keeps the strongest observation for each SSID.
The status projects network association and a usable address/default route on the Wi-Fi link. The
agent's connection is observed separately. Helper, Binder and transport failures become
`wifi-unavailable`. Missing endpoints and unsupported versions report `wifi-unsupported`. A malformed
device request reports `wifi-invalid-request`.

The device owns one transaction at a time, with `wifi-busy` for a competing join or forget. The
Android provider snapshots the current link and any profile the join would change before applying
the candidate. The helper keeps any required prior credential inside its process. It confirms a
fresh association with the selected profile and a usable route. A failed candidate is removed or
replaced with the saved profile, and the provider verifies the restored link before returning
`wifi-join-failed`. An unverified restore returns `wifi-rollback-failed`. Both results carry the
observed SSID and reachability. Forget targets one saved network ID and returns `wifi-not-saved`
for a stale selection. The device completes admitted mutations if the requesting client leaves.
The paired page sends the join password in the request body and clears its password field on submit.

The loopback screen server relays these frames through `POST /_screen/wifi` with the existing pairing
token, exact listener Origin and JSON media type. The server bounds the request and sends empty HTTP
errors for rejected input. Results carry `Cache-Control: no-store` and `Referrer-Policy: no-referrer`.
An unsupported result leaves the existing Settings available while the fork remains selectable.

On a paired page with Wi-Fi support, the screen polls device reachability. A continuous confirmed
loss opens Settings with Wi-Fi expanded and focus on its summary after 30 seconds. An unknown
result clears the pending deadline. After dismissal, the next prompt requires a confirmed recovery
and another full outage. The local page and its Wi-Fi relay remain available during network loss.

The coordinator assigned the USB fallback to this lane under option B. `echo-setup wifi status` and
`echo-setup wifi join --ssid NAME` select a ready USB device and run the fixed
`/data/local/echo/bin/current wifi` entry point over non-PTY adb. Status closes stdin. Join reads
one password line from a file or redirected stdin and sends the v1 frame on adb stdin. The
device command uses the same endpoint and transaction as the paired page. The setup utility
validates the framed response and emits its own result. Fork installs retain their documented
`scripts/wifi-join.sh` fallback until the coordinator's A2 T5 cutover.

## Deferred

- The coordinator proves the installed helper's system UID, Binder/SELinux admission, Wi-Fi route
  observations and saved-profile restoration at R-T5, and owns the A2 cutover.
- The HOME waiting view remains the recovery surface when the core is stopped. Serving a page without
  the core belongs outside this Wi-Fi slice.
- Light band, installer and boot supervision, unrelated Settings additions and all phase-wide boot
  and allocation outcomes retain their own plan owners.

[fork]: https://github.com/portpowered/echo-dot-assist/blob/7e0c758d3bb352b06aafce64104354ad67b22f84/tools/mkwifictl.py
