# The Android app

The README's [Android app](../README.md#android-app) covers installing and
pairing the app, and [Remote access](../README.md#remote-access) what it
connects to. This page covers updates, notification delivery, and
building it yourself.

## Updates

The app is of pm's own version: Settings shows it beside the server's, and
warns when the two are of different releases. It checks GitHub for a newer
release as it starts and daily (Settings turns that off, or checks now); a
newer one comes as a notification whose tap downloads its APK, which
installs over the app.

## Notifications

Notifications reach the app off the tailnet through a UnifiedPush
distributor. Each time the app opens and reaches the server, it registers
through the distributor it used before — or, if that one is gone, the
phone's default, else any installed one, else Google's push service built
into the app (which needs Google Play services) — and sends the server its
subscription; Settings switches between them. While the app holds no
subscription — no distributor gave it one, or registering failed, as
Google's does on a phone without Play services — it polls the server
instead, which works only on the tailnet and only as often as Android lets
it: every 15 minutes at best, and hours apart once the phone dozes or the
app goes unused. [Push](remote-api.md#push) has the server side.

## Building

The app lives in `android/` (Kotlin, Jetpack Compose), and needs JDK 17+
and the Android SDK with platform 37. The build finds the SDK through
`ANDROID_HOME` or `sdk.dir` in `android/local.properties`; with neither, it
writes the latter for `~/Library/Android/sdk`, where Android Studio
installs it on macOS.

```sh
android/gradlew -p android assembleGoogleRelease
adb install -r android/app/build/outputs/apk/google/release/app-google-release.apk
```

The `google` flavour is the one released; `fdroid` (`assembleFdroidRelease`)
is the same app without Google's push service or the GitHub update check,
so it has no proprietary code and leaves updates to F-Droid.

The release build is shrunk by R8 and carries only `arm64-v8a` code (the
debug build adds `x86_64` for an emulator). It is signed only when the
Gradle property `pmReleaseSigning` (in `~/.gradle/gradle.properties`, or
`ORG_GRADLE_PROJECT_pmReleaseSigning`) names a properties file with
`storeFile`, `storePassword`, `keyAlias` and `keyPassword`; otherwise it
builds an `-unsigned.apk`, which a phone refuses.

A phone installs an update only if it is signed with the key the installed
app was, so every release is signed with the one key; an APK signed with
another installs only after an uninstall, which drops the app's pairing.
For the same reason, installing a release over the debug build
(`assembleDebug`, signed with the SDK's debug key) needs one `adb uninstall
dev.pm.app` and a new pairing. The version code is the minute of the last
commit, so a build of an earlier commit cannot replace a later one, a
release included.
