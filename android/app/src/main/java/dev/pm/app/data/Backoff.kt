package dev.pm.app.data

import kotlin.random.Random
import kotlin.time.Duration
import kotlin.time.Duration.Companion.seconds

/**
 * Waits between reconnects: doubling from `first` up to `max`, each with
 * up to half taken off at random so that retries don't fall in step.
 */
class Backoff(
    private val first: Duration = 2.seconds,
    private val max: Duration = 30.seconds,
    private val random: Random = Random.Default,
) {
    private var step = first

    fun next(): Duration {
        val wait = step * (1 - random.nextDouble() / 2)
        step = (step * 2).coerceAtMost(max)
        return wait
    }

    fun reset() {
        step = first
    }
}
