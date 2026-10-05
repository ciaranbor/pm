package dev.pm.app.update

import kotlinx.coroutines.runBlocking
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import okhttp3.OkHttpClient
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

class UpdatesTest {
    private val server = MockWebServer()
    private lateinit var updates: Updates

    @Before
    fun start() {
        server.start()
        updates = Updates(OkHttpClient(), server.url("/releases/latest").toString())
    }

    @After
    fun stop() {
        server.close()
    }

    private fun latest(tag: String, vararg apks: String) {
        val assets =
            (apks.toList() + "pm-aarch64-apple-darwin" + "SHA256SUMS").joinToString(",") {
                """{"name":"$it","browser_download_url":"https://example.com/$tag/$it","size":1}"""
            }
        server.enqueue(
            MockResponse.Builder()
                .body("""{"tag_name":"$tag","prerelease":false,"assets":[$assets]}""")
                .build()
        )
    }

    @Test
    fun a_newer_release_offers_the_apk_of_the_first_abi_it_has() = runBlocking {
        latest("v0.3.0", "pm-android-arm64-v8a.apk", "pm-android-x86_64.apk")
        assertEquals(
            Update("0.3.0", "https://example.com/v0.3.0/pm-android-x86_64.apk"),
            updates.check("0.2.0", listOf("riscv64", "x86_64", "arm64-v8a")),
        )
        assertEquals(
            "application/vnd.github+json",
            server.takeRequest().headers["Accept"],
        )
    }

    @Test
    fun the_same_or_an_older_release_or_one_without_an_apk_for_this_phone_offers_nothing() =
        runBlocking {
            latest("v0.2.0", "pm-android-arm64-v8a.apk")
            assertNull(updates.check("0.2.0+3.gabc1234.dirty", listOf("arm64-v8a")))
            latest("v0.2.0", "pm-android-arm64-v8a.apk")
            assertNull(updates.check("0.10.0", listOf("arm64-v8a")))
            latest("v0.3.0", "pm-android-arm64-v8a.apk")
            assertNull(updates.check("0.2.0", listOf("x86_64")))
        }

    @Test
    fun versions_compare_by_semver_precedence() {
        assertEquals(0, Updates.compare("0.2.0+3.gabc", "0.2.0"))
        assertTrue(Updates.compare("0.2.0-rc.1", "0.2.0") < 0)
        assertTrue(Updates.compare("0.2.0-rc.2", "0.2.0-rc.10") < 0)
        assertTrue(Updates.compare("0.2.0-alpha", "0.2.0-alpha.1") < 0)
        assertTrue(Updates.compare("0.10.0", "0.9.9") > 0)
    }
}
