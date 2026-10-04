pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral()
    }
}

rootProject.name = "pm-android"
include(":app")

val localProperties = file("local.properties")
if (
    !localProperties.exists() &&
        !providers.environmentVariable("ANDROID_HOME").isPresent &&
        !providers.environmentVariable("ANDROID_SDK_ROOT").isPresent
) {
    val sdk = File(providers.systemProperty("user.home").get(), "Library/Android/sdk")
    if (sdk.isDirectory) localProperties.writeText("sdk.dir=${sdk.path}\n")
}
