# Echo Screen

The single API-28 Activity opens the bridge's page URL, keeps the display awake,
and reserves 16 physical pixels above and 10 below the WebView for hassmic.
JavaScript and DOM storage are enabled. The URL stays in private preferences;
backup is disabled. The Activity never logs URLs or WebView error descriptions.

Use `make build-echo-screen` with an existing `ANDROID_HOME` containing
`build-tools/35.0.0` and `platforms/android-34/android.jar`. The script uses the
SDK tools and an installed JDK (preferring `JAVA_HOME_17_X64`); it downloads
nothing. The output is `target/echo-screen/echo-screen.apk`. Run
`python tests/verify_echo_screen_apk.py` to check its package, API levels,
INTERNET permission, alignment and signature with the SDK tools.

By default, a build-only signing key is created and reused under the ignored
`target/echo-screen/` directory. For a durable installation identity, set
`ECHO_SCREEN_KEYSTORE` to an existing operator keystore; apksigner prompts for
its password. Keep that key outside the repository. Losing the signing key means
an existing installation cannot be updated with `install -r` using a new key.
Build work directories and private tool logs also stay under `target/echo-screen/`.
A failed build leaves the previously published APK intact.
On Git Bash with the Windows SDK, the build invokes the SDK's d8/apksigner jars
directly instead of their batch launchers; the SDK inputs and API level match CI.

Launch component `org.portos.echoscreen/.MainActivity` with string extra `url`
containing the entire paired page URL, including nested `events` and `actions`
parameters. The app does not assemble the URL. The server's `page_url` helper
returns it wrapped in `Secret<String>`. An absent extra restores the last valid
URL; an invalid extra shows a fixed error and retains the saved configuration.
Without configuration, the Activity shows `Screen URL is not configured`.

Main-frame initial load failures show `Waiting for bridge` instead of Chromium's
error page and retry at 1, 2, 4, 8, 16, 30, 30... seconds. After a successful load,
the page owns reconnect behavior. A replacement intent or Activity destruction
cancels pending work. Each native attempt uses a fresh WebView, isolating stale
callbacks. No JavaScript bridge is installed.

`make test-echo-screen-jvm` runs network-free policy tests with a fake scheduler
and preference store. `make test-echo-screen-url` checks exact nested encoding
and the page parser's origin rules without opening sockets. These tests do not
prove Android rendering, real preference permissions, touch accuracy, boot or
overlay coexistence. Those remain the operator's later M1 Android gate; factory
workers must never install this APK on hardware.

The coordinator uses `sh scripts/echo-screen-install.sh --url "$PAGE_URL"`
after building the APK. It installs with `adb install -r`, writes hassmic's
existing `screen_home` file (package plus LF, system:system, mode 644), and
starts the Activity. Without `--url`, the Activity restores its saved URL.
Only absolute HTTP(S) URLs without credentials or whitespace are accepted.
The URL is quoted for adb's remote shell; adb output is private and removed
after execution. Do not use shell tracing, which would disclose the URL.

`sh scripts/echo-screen-install.sh --uninstall` removes `screen_home` before
uninstalling the package. Each operation stops on the first failure. A failed
home setup or launch can leave a partially configured installation; fix the
cause and rerun, or uninstall. The script does not restore an unknown previous
home app. On Git Bash it disables MSYS argument conversion for device paths
and converts only the local APK path for native adb.

`make test-echo-screen-install` builds the existing fake adb once and exercises
the real installer with isolated per-case PATH, APK, fixture and argv ledger.
These controlled CLI tests never execute an installed adb or contact hardware.
`make check-echo-screen` builds the APK, runs the JVM, URL and installer tests,
then inspects the prebuilt package with aapt2, zipalign and apksigner. The
existing unconditional `build-host` CI job runs this same target; missing SDK
inputs fail rather than installing packages or skipping checks.
