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

val releaseSigning = providers.gradleProperty("pmReleaseSigning").map { file(it) }

android {
    namespace = "dev.pm.app"
    compileSdk = 37

    defaultConfig {
        applicationId = "dev.pm.app"
        minSdk = 26
        targetSdk = 37
        versionCode = commitMinutes.get()
        versionName = "0.1.0"
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

    buildFeatures { compose = true }

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
        // Release ships arm64 only, to keep ML Kit's native scanner to one ABI.
        disable += "ChromeOsAbiSupport"
        abortOnError = true
    }
}

configurations.configureEach {
    // The UnifiedPush connector's tink and ML Kit's must agree.
    val tink = "com.google.crypto.tink:tink-android:1.20.0"
    resolutionStrategy {
        force(tink)
        dependencySubstitution {
            substitute(module("com.google.crypto.tink:tink")).using(module(tink))
        }
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
    implementation(libs.mlkit.barcode)
    implementation(libs.markdown.m3)
    implementation(libs.unifiedpush.connector)
    implementation(libs.unifiedpush.fcm)

    lintChecks(libs.compose.lint.checks)

    testImplementation(libs.junit)
    testImplementation(libs.kotlinx.coroutines.test)
    testImplementation(libs.okhttp.mockwebserver)
    testImplementation(libs.robolectric)
    testImplementation(libs.compose.ui.test.junit4)
    testImplementation(libs.roborazzi)
    testImplementation(libs.roborazzi.compose)
    debugImplementation(libs.compose.ui.test.manifest)
}

roborazzi { outputDir.set(file("src/test/screenshots")) }

ktfmt { kotlinLangStyle() }
