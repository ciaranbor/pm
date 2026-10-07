package dev.pm.app.push

import dev.pm.app.api.PmError
import kotlin.time.Duration
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.TimeoutCancellationException
import kotlinx.coroutines.withTimeout

/** A send from a notification's action, which has seconds before Android may kill the process. */
internal object Sending {
    /** `send`'s result within `within`, else what `failed` makes of why not. */
    suspend fun <T> attempt(
        within: Duration,
        failed: (String) -> T,
        send: suspend () -> T,
    ): T =
        try {
            withTimeout(within) { send() }
        } catch (e: TimeoutCancellationException) {
            failed("no answer in time")
        } catch (e: CancellationException) {
            throw e
        } catch (e: PmError.Unreachable) {
            failed("pm serve unreachable")
        } catch (e: Exception) {
            failed(e.message ?: e.javaClass.simpleName)
        }
}
