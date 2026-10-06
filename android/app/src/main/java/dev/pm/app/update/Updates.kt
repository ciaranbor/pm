package dev.pm.app.update

import java.io.IOException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import okhttp3.OkHttpClient
import okhttp3.Request

/** A release newer than this app, and the APK of it to install. */
data class Update(val version: String, val apk: String)

/**
 * Whether pm has released a newer app, asked of GitHub's latest release, which is never a
 * prerelease. Each release carries one APK per ABI, `pm-<version>-android-<abi>.apk`, signed with
 * the key every pm APK is, so Android installs it over this one.
 */
class Updates(
    private val http: OkHttpClient,
    private val latest: String = LATEST,
) {
    /**
     * The latest release if it is newer than `current`, for the first of `abis` it has an APK for.
     */
    suspend fun check(current: String, abis: List<String>): Update? {
        val release = withContext(Dispatchers.IO) { fetch() }
        val version = release.tag.removePrefix("v")
        if (compare(version, current) <= 0) return null
        val apk =
            abis.firstNotNullOfOrNull { abi ->
                release.assets.find { it.name == "pm-$version-android-$abi.apk" }
            } ?: return null
        return Update(version, apk.url)
    }

    private fun fetch(): Release {
        val request =
            Request.Builder().url(latest).header("Accept", "application/vnd.github+json").build()
        http.newCall(request).execute().use { response ->
            if (!response.isSuccessful) throw IOException("GitHub answered ${response.code}")
            return json.decodeFromString(Release.serializer(), response.body.string())
        }
    }

    @Serializable
    private data class Release(
        @SerialName("tag_name") val tag: String,
        val assets: List<Asset> = emptyList(),
    )

    @Serializable
    private data class Asset(
        val name: String,
        @SerialName("browser_download_url") val url: String,
    )

    companion object {
        const val LATEST = "https://api.github.com/repos/ciaranbor/pm/releases/latest"

        private val json = Json { ignoreUnknownKeys = true }

        /**
         * How semver `a` orders against `b`: build metadata (`+…`, a build from source) is ignored,
         * and a prerelease precedes its release. A part that isn't a number orders as 0.
         */
        fun compare(a: String, b: String): Int {
            fun parse(v: String): Pair<List<Int>, String?> {
                val bare = v.substringBefore('+')
                val pre = bare.substringAfter('-', "").ifEmpty { null }
                val core = bare.substringBefore('-').split('.').map { it.toIntOrNull() ?: 0 }
                return (core + List(3) { 0 }).take(3) to pre
            }
            val (coreA, preA) = parse(a)
            val (coreB, preB) = parse(b)
            coreA.zip(coreB).forEach { (x, y) -> if (x != y) return x.compareTo(y) }
            return when {
                preA == preB -> 0
                preA == null -> 1
                preB == null -> -1
                else -> comparePre(preA, preB)
            }
        }

        /** Semver's precedence of two prerelease parts: field by field, numbers below words. */
        private fun comparePre(a: String, b: String): Int {
            val x = a.split('.')
            val y = b.split('.')
            x.zip(y).forEach { (p, q) ->
                val np = p.toIntOrNull()
                val nq = q.toIntOrNull()
                val c =
                    when {
                        np != null && nq != null -> np.compareTo(nq)
                        np != null -> -1
                        nq != null -> 1
                        else -> p.compareTo(q)
                    }
                if (c != 0) return c
            }
            return x.size.compareTo(y.size)
        }
    }
}
