package dev.pm.app.data

import kotlin.random.Random
import kotlin.time.Duration.Companion.seconds
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class BackoffTest {
    private class Fixed(private val value: Double) : Random() {
        override fun nextBits(bitCount: Int) = 0

        override fun nextDouble() = value
    }

    @Test
    fun waits_double_up_to_the_cap_and_start_over_on_reset() {
        val none = Backoff(random = Fixed(0.0))
        assertEquals(listOf(2, 4, 8, 16, 30, 30).map { it.seconds }, List(6) { none.next() })
        none.reset()
        assertEquals(2.seconds, none.next())
    }

    @Test
    fun jitter_takes_off_at_most_half() {
        val most = Backoff(random = Fixed(0.999999))
        assertTrue(most.next() > 1.seconds)
        val random = Backoff()
        repeat(100) {
            random.reset()
            val wait = random.next()
            assertTrue("$wait", wait > 1.seconds && wait <= 2.seconds)
        }
    }
}
