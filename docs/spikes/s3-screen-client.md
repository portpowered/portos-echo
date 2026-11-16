---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 15
status: measured on the Echo (build 7809, hassmic 8646a4f); the run-time overlays and the LAN path remain M1 T5 items
---

# S3: the screen client

Recommend **our own minimal WebView app (`echo-screen`) as hassmic's home app**. No Kiosk Satellite fallback is
needed. A 12 KB throwaway APK with a single Activity, a full-screen `WebView` and nothing else did each of these on
the Echo:

- It showed `echo-screen-server`'s page.
- It started on boot as hassmic's home app.
- It sent touches back as `UiAction`s.
- It played the `Emotion` animation.
- It ran beside hassmic, whose self test still passed.

The spike's code stays out of the repository. The item that builds `echo-screen` for real starts from the shape below.

## Setup

- **The Echo:** Echo Show 8 2nd gen (athena), Fire OS build 7809 (Android 9, API 28), with hassmic 2026.11.13.185908
  (`8646a4f`) installed as `hassmic -P wyoming -D -E` and Alexa off.
- **The WebView:** `com.amazon.webview.chromium` 138 (`138.amazon-webview-v138-7204.7204.244.276`).
- **The server:** `echo-screen-server`'s `screen_fixture` example, built from `main` (`01f732e`) with `web/screen`'s
  bundle. It is bound to `127.0.0.1:28940` on the operator's PC, with a fresh random pairing token.
- **The tunnel:** the Echo reached the server through `adb reverse tcp:28940 tcp:28940`, so nothing listened beyond
  loopback.
- **States and actions:** pushed as JSON lines on the fixture's stdin. The fixture's stdout shows the `UiAction`s it
  received.
- **The app:** package `org.portos.echoscreen` with one Activity.
  - `WebView` with JavaScript and DOM storage on, cleartext allowed (the bridge's page is plain HTTP).
  - Immersive full screen, and the screen kept on.
  - The URL comes from an intent extra and is saved in `SharedPreferences`.
  - It logs the URL with the token redacted.
- **The build:** no Gradle. The Android SDK's own tools did it: `aapt2 link` (manifest only, `android-30`'s
  `android.jar`, min and target SDK 28), `javac --release 8`, `d8 --min-api 28`, `zipalign` and `apksigner` with a
  throwaway key.

## Measurements

| What | Result |
| --- | --- |
| The page | Shows `Ready` / "Echo Live ready" after the server's first state, and `Emotion` "happy" with its frames |
| Animation | Two screen captures taken back to back differ, and the frame text advances (`(^_^)`, `(^o^)`) at the 200 ms `frame_ms` sent |
| Touch | `adb shell input tap` on Start gives `{"action":"tap","target":"primary"}` at the server; on Dismiss, `{"action":"dismiss"}` |
| Screen area | hassmic gives the app 1280×774 of the 1280×800 panel: a 16 px grip at the top (its drawer's pull strip) and its 10 px light band at the bottom, both drawn over every app (`boot.log`: `screen: 1280x774 between a grip of 16 and a band of 10`) |
| Start on boot | With `screen_home` set to `org.portos.echoscreen`, the app had focus 33 s after `adb reboot`, about 3 s after `sys.boot_completed`. The page reconnected on its own and showed the state the server held |
| hassmic beside it | After the reboot: `wyoming on 16700, wake=local`, `self test: passed`, LISTEN on 16700 |
| Memory, hassmic's face only | Used RAM 630 MB, Free RAM 1,224 MB of 1,974 MB (`dumpsys meminfo`) |
| Memory, the app showing the page | Used 852 MB, Free 1,031 MB. The app's PSS is 70 MB (Java heap 1.8 MB, native heap 7.1 MB); the WebView renderer `sandboxed_process0` has 164 MB RSS |
| Memory, the app as home after a reboot | Used 805 MB, Free 1,079 MB |
| First start | On the very first cold start, a capture 9 s after launch was still black. Every later start drew within 3 s |

## What the item that builds `echo-screen` must know

1. **The page's URL carries two parameters, `events` and `actions`.** Each is a same-origin URL that holds the
   pairing token, URL-encoded: `/?events=%2Fevents%3Ftoken%3D<t>&actions=%2Factions%3Ftoken%3D<t>`. A bare
   `/?token=` gives "Screen configuration is invalid" (`web/screen/src/transport.ts:3-18`). The bridge should print
   or hand over this whole URL. The app should not assemble it.
2. **Set the home app** through hassmic's settings page: the Screen page's home app, `POST /api/screen` with
   `home <package>`, which is signed. hassmic keeps the choice in `/data/local/hassmic/state/screen_home` (one
   line). A new choice applies at once.
3. **Keep the app's layout clear of the top 16 px and the bottom 10 px.** hassmic draws over them: the drawer opens
   from the grip, and "back" is a swipe along the band.
4. **Retry a failed load.** In production the server is the bridge on the operator's PC, which can start after the
   Echo. The spike's page recovered because the server was already up when the app started. A load that fails
   (connection refused) must be retried with backoff, never left on Chromium's error page.
5. **The pairing token is a secret:** never log it. The app redacts it. The URL is kept in the app's private
   `SharedPreferences` (`/data/data/<pkg>`), the same exposure as Kiosk Satellite's configuration.
6. **Build without Gradle.** The APK is a manifest and one class, so `aapt2`, `javac`, `d8`, `zipalign` and
   `apksigner` from the Android SDK are enough, with no Gradle and no new dependencies. A CI job needs an Android
   SDK with build-tools 35 and `platforms;android-30`, or any platform at API 28 or later. The signing key is never
   committed: CI signs with a throwaway debug key, and the operator signs the APK that goes on the Echo with a key
   of their own.

## Not measured here

- hassmic's face and words layers over the app during a run (a wake, then a reply). The plan's coexistence check
  with the light band covers the idle band only. This needs a run, so it moves to M1's T5 run.
- The Echo reaching the bridge's screen server over the LAN, rather than through `adb reverse`. hassmic's lockdown
  limits Amazon's programs to the LAN and leaves other apps alone. The M1 T5 run checks it.
- Touch with a finger. `input tap` enters at the input dispatcher. Real finger accuracy belongs to ADR 0019's touch
  calibration.
- Long-run memory growth. The numbers above are single readings minutes apart.
