package dev.pm.app.ui

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dev.pm.app.api.PmClient
import dev.pm.app.api.PmError
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch

/**
 * Why a feature's merge would not go through now, in the server's few words; `null` when it would,
 * or when the server can't say (it predates the check, or is unreachable), in which case Merge
 * stays on and the server refuses a merge it can't make. Asked again on each [refresh], since the
 * server checks git only when asked.
 */
class MergeCheckModel(
    private val client: PmClient?,
    private val project: String,
    private val feature: String,
) : ViewModel() {
    private val _blocker = MutableStateFlow<String?>(null)
    val blocker: StateFlow<String?> = _blocker.asStateFlow()

    private var checking: Job? = null
    private var again = false

    fun refresh() {
        val client = client ?: return
        if (checking?.isActive == true) {
            again = true
            return
        }
        checking = viewModelScope.launch {
            do {
                again = false
                try {
                    val check = client.mergeCheck(project, feature)
                    _blocker.value =
                        if (check.mergeable) null else check.reason ?: "Can't merge now"
                } catch (e: CancellationException) {
                    throw e
                } catch (e: PmError.Unsupported) {
                    _blocker.value = null
                } catch (e: Exception) {
                    // Unknown for now; the last answer stands until the next.
                }
            } while (again)
        }
    }
}
