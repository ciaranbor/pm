import java.util.Properties

plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.compose)
    alias(libs.plugins.kotlin.serialization)
    alias(libs.plugins.roborazzi)
    alias(libs.plugins.ktfmt)
}

val commitMinutes =
    providers
        .exec {
            commandLine("git", "log", "-1", "--format=%ct", "HEAD")
            isIgnoreExitValue = true
        }
        .standardOutput
        .asText
        .map { it.trim().toLongOrNull()?.let { seconds -> (seconds / 60).toInt() } ?: 1 }

/** Set by CI: a release build, named by Cargo.toml's version alone and signed. */
val pmRelease = providers.gradleProperty("pmRelease").isPresent

/** pm's version, which the app shares: Cargo.toml's `[package]` version. */
val cargoVersion =
    providers.fileContents(rootProject.layout.projectDirectory.file("../Cargo.toml")).asText.map {
        Regex("""(?m)^\[package\][^\[]*?^version\s*=\s*"([^"]+)"""").find(it)?.groupValues?.get(1)
            ?: error("Cargo.toml has no [package] version")
    }

/**
 * A build that isn't a release appends what `git describe` says of the checkout, as pm's `build.rs`
 * does: commits since the last `v*` tag, the commit, and whether the tree is dirty.
 */
val buildMetadata =
    providers
        .exec {
            commandLine(
                "git",
                "describe",
                "--tags",
                "--long",
                "--dirty",
                "--match",
                "v*",
                "--always",
            )
            isIgnoreExitValue = true
        }
        .standardOutput
        .asText
        .map { described ->
            val dirty = described.trim().endsWith("-dirty")
            val clean = described.trim().removeSuffix("-dirty")
            val fields = clean.split("-")
            val parts =
                when {
                    clean.isEmpty() -> emptyList()
                    fields.size >= 3 && fields.last().startsWith("g") ->
                        listOf(fields[fields.size - 2], fields.last())
                    else -> listOf("g$clean")
                } + if (dirty) listOf("dirty") else emptyList()
            parts.joinToString(".")
        }

val releaseSigning = providers.gradleProperty("pmReleaseSigning").map { file(it) }

if (pmRelease && !releaseSigning.isPresent) {
    error("-PpmRelease needs -PpmReleaseSigning: a release is never published unsigned")
}

android {
    namespace = "dev.pm.app"
    compileSdk = 37

    defaultConfig {
        applicationId = "dev.pm.app"
        minSdk = 26
        targetSdk = 37
        versionCode = commitMinutes.get()
        versionName =
            cargoVersion.get().let { base ->
                if (pmRelease) base
                else buildMetadata.get().let { if (it.isEmpty()) base else "$base+$it" }
            }
    }

    signingConfigs {
        if (releaseSigning.isPresent) {
            val path = releaseSigning.get()
            val signing = Properties().apply { path.reader().use(::load) }
            fun required(key: String) =
                signing.getProperty(key) ?: error("pmReleaseSigning file $path lacks $key")
            create("release") {
                storeFile = path.parentFile.resolve(required("storeFile"))
                storePassword = required("storePassword")
                keyAlias = required("keyAlias")
                keyPassword = required("keyPassword")
            }
        }
    }

    buildTypes {
        debug { ndk { abiFilters += listOf("arm64-v8a", "x86_64") } }
        release {
            optimization {
                enable = true
                keepRules { files.add(file("keep-rules.pro")) }
            }
            ndk { abiFilters += "arm64-v8a" }
            signingConfig = signingConfigs.findByName("release")
        }
    }

    // The published build carries Google's push service as a UnifiedPush distributor for phones
    // without one; fdroid has no proprietary code, so only a distributor such as ntfy, or
    // polling, notifies.
    flavorDimensions += "distribution"
    productFlavors {
        create("google") {
            dimension = "distribution"
            isDefault = true
            buildConfigField("boolean", "SELF_UPDATE", "true")
        }
        // F-Droid builds, signs and updates its own APK, so this one never offers GitHub's.
        create("fdroid") {
            dimension = "distribution"
            buildConfigField("boolean", "SELF_UPDATE", "false")
        }
    }

    buildFeatures {
        compose = true
        buildConfig = true
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    testOptions {
        unitTests.isIncludeAndroidResources = true
        // Robolectric reaches FileDescriptor internals through SharedSecrets.
        unitTests.all { it.jvmArgs("--add-exports=java.base/jdk.internal.access=ALL-UNNAMED") }
    }

    lint {
        warningsAsErrors = true
        // Release ships arm64 only, the one APK a release publishes.
        disable += "ChromeOsAbiSupport"
        abortOnError = true
    }
}

dependencies {
    implementation(libs.androidx.core.ktx)
    implementation(libs.androidx.activity.compose)
    implementation(libs.androidx.lifecycle.runtime.compose)
    implementation(libs.androidx.lifecycle.viewmodel.compose)
    implementation(libs.androidx.navigation3.runtime)
    implementation(libs.androidx.navigation3.ui)
    implementation(libs.androidx.lifecycle.viewmodel.navigation3)
    implementation(platform(libs.compose.bom))
    implementation(libs.compose.ui)
    implementation(libs.compose.ui.tooling.preview)
    implementation(libs.compose.material3)
    debugImplementation(libs.compose.ui.tooling)
    implementation(libs.kotlinx.coroutines.android)
    implementation(libs.kotlinx.serialization.json)
    implementation(libs.okhttp)
    implementation(libs.okhttp.sse)
    implementation(libs.camera.camera2)
    implementation(libs.camera.lifecycle)
    implementation(libs.camera.view)
    implementation(libs.zxing.core)
    implementation(libs.androidx.work.runtime)
    implementation(libs.markdown.m3)
    implementation(libs.unifiedpush.connector)
    "googleImplementation"(libs.unifiedpush.fcm)

    lintChecks(libs.compose.lint.checks)

    testImplementation(libs.junit)
    testImplementation(libs.kotlinx.coroutines.test)
    testImplementation(libs.okhttp.mockwebserver)
    testImplementation(libs.androidx.work.testing)
    testImplementation(libs.robolectric)
    testImplementation(libs.compose.ui.test.junit4)
    testImplementation(libs.roborazzi)
    testImplementation(libs.roborazzi.compose)
    testImplementation(libs.roborazzi.accessibility.check)
    debugImplementation(libs.compose.ui.test.manifest)
}

roborazzi { outputDir.set(file("src/test/screenshots")) }

ktfmt { kotlinLangStyle() }
