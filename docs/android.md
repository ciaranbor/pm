# The Android app

The README's [Android app](../README.md#android-app) covers installing and
pairing the app, and [Remote access](../README.md#remote-access) what it
connects to. This page covers updates, notification delivery, a project's
actions, a feature's workspace, and building it yourself.

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

A push says only which scope entered which kind, so the app shows that at
once, then fills in what the server says when the tailnet answers within
10 s: the question or command an asking agent's oldest open dialog puts, a
blocked feature's reason, a ready one's summary. Each notification is
titled by its feature (a `main` by its project) and grouped under its
project, and opens its agent. A permission prompt also has Allow and Deny,
answered as the dialog card answers it (Deny stops the agent's turn), and
a feature blocked on you has an inline reply to the agent that blocked it;
both need the phone unlocked. What you sent shows in the notification
until it is withdrawn, silently, once its need is over — answered at the
terminal, say. The app learns that from the server's end push even while
closed, and while open, on each push and each poll.

Polling alerts what a push would: a scope's top-ranked need as it begins,
so a ready feature whose agent asks alerts the question first and ready
once it is answered.

## Projects

A project's page has Open, Close and Delete in its top bar: Open
while any of its sessions is missing, Close while any is up. Open runs at
once. Close asks first, and says how many agents are mid-turn; Open brings
them back, each resuming its conversation. Delete runs `pm delete` without
`--force` once you type the project's name, and leaves every page of the
project. The start screen lists a project with no session up under Closed
projects, after the rest; its page offers Open. A registered project that
isn't on this machine is not listed
([snapshot](remote-api.md#json-version-1)).

Beside its Notes, a project's page has Docs: the categories of its
[information store](../README.md#information-store-and-summaries), each with
what it holds, its size and when it last changed, each opening to read. The
docs are read-only on the phone, since `main` writes them. Sections in the
top bar jumps to a heading. A server that predates the docs shows no Docs button.

## Features

A feature's workspace is its agents' chats, with a tab per agent once it
has two. The ⓘ in its top bar opens the feature's page: where it stands
(what it needs, its status and PR, its activity) over its summary, brief
and details; Up returns to the chat. While the feature is ready for
review, a strip above the composer says so and leads to that page, whose
Merge button sits at the bottom. A feature with no agents running opens
straight on its page. Merge, Delete, and the shown agent's terminal and
restart are in the chat's ⋮ menu.

A feature's Merge is off while merging would not land its branch as it
stands, with the reason under the menu's Merge, and on the feature's page
under where it stands, or above its Merge button once it is ready:
uncommitted changes, a paused merge or rebase, or a branch behind its
base, which needs a rebase at a terminal first. The app
asks each time the feature's chat or page shows, or its state changes,
and every 25 s while its page is on screen, so a commit at a terminal
shows without reopening it. A server too old to say leaves Merge on and
refuses a merge it can't make.

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

To run an unreleased build on a phone that has the released app,
`scripts/phone install` (`--help`) builds this checkout signed with pm's key
and installs it over the app, keeping its pairing.
