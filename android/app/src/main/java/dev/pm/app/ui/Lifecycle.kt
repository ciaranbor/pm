package dev.pm.app.ui

import dev.pm.app.api.PmClient
import dev.pm.app.api.PmError
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.receiveAsFlow
import kotlinx.coroutines.launch

/** What the app can ask `pm serve` to do to a feature or an agent, with the `lifecycle` grant. */
sealed interface Action {
    val project: String

    /** What it acts on: the feature's or the agent's name. */
    val subject: String

    /** The verb, as a button names it. */
    val verb: String

    data class Merge(override val project: String, val feature: String) : Action {
        override val subject = feature
        override val verb = "Merge"
    }

    data class Delete(override val project: String, val feature: String) : Action {
        override val subject = feature
        override val verb = "Delete"
    }

    data class Restart(
        override val project: String,
        val scope: String,
        val agent: String,
        val force: Boolean = false,
    ) : Action {
        override val subject = agent
        override val verb = "Restart"
    }
}

/** Whether this device may merge, delete and restart. */
sealed interface Grants {
    /** Not asked yet, or the server couldn't be reached: an action finds out. */
    data object Unknown : Grants

    data object Granted : Grants

    /** The device lacks the grant; `device` is its name, which the grant command takes. */
    data class NotGranted(val device: String) : Grants

    /** The server predates grants. */
    data object Unsupported : Grants
}

/** Where an action stands. */
sealed interface ActionState {
    data object Idle : ActionState

    /** Asked to confirm a merge, a delete, or a restart that would interrupt a turn. */
    data class Confirming(val action: Action) : ActionState

    data class Running(val action: Action) : ActionState

    /** It didn't end as asked; `reason` says why, in the server's words. */
    data class Failed(
        val action: Action,
        val reason: String,
        val outcome: Outcome = Outcome.Refused,
    ) : ActionState
}

/** How much of a failed action happened. */
enum class Outcome {
    /** Nothing: it was refused. */
    Refused,

    /** Unknown: the server stopped answering. */
    Lost,

    /** Possibly part of it: it failed on the way. */
    Broken,
}

/** The command that grants this device lifecycle actions, run on the Mac. */
fun grantCommand(device: String) = "pm serve grant $device lifecycle"

/**
 * Lifecycle actions: confirmed where they destroy or interrupt, run one at a time, with their
 * refusals kept as the server words them. A merge or delete that went through is sent on
 * [finished].
 */
class Lifecycle(private val scope: CoroutineScope, private val client: () -> PmClient?) {
    private val _grants = MutableStateFlow<Grants>(Grants.Unknown)
    val grants: StateFlow<Grants> = _grants.asStateFlow()

    private val _state = MutableStateFlow<ActionState>(ActionState.Idle)
    val state: StateFlow<ActionState> = _state.asStateFlow()

    private val _finished = Channel<Action>(Channel.BUFFERED)

    /** Each action that went through, once. */
    val finished: Flow<Action> = _finished.receiveAsFlow()

    /** Ask the server what this device was granted. */
    fun refreshGrants() {
        val client = client() ?: return
        scope.launch {
            _grants.value =
                try {
                    when (val device = client.device()) {
                        null -> Grants.Unsupported
                        else ->
                            if (device.lifecycle) Grants.Granted else Grants.NotGranted(device.name)
                    }
                } catch (e: CancellationException) {
                    throw e
                } catch (e: Exception) {
                    Grants.Unknown
                }
        }
    }

    /** Start `action` from its menu: a restart runs at once, a merge or delete asks first. */
    fun ask(action: Action) {
        if (_state.value is ActionState.Running) return
        when (action) {
            is Action.Restart -> run(action)
            else -> _state.value = ActionState.Confirming(action)
        }
    }

    fun confirm() {
        (_state.value as? ActionState.Confirming)?.let { run(it.action) }
    }

    fun dismiss() {
        if (_state.value !is ActionState.Running) _state.value = ActionState.Idle
    }

    private fun run(action: Action) {
        val client = client() ?: return
        _state.value = ActionState.Running(action)
        scope.launch {
            _state.value =
                try {
                    when (action) {
                        is Action.Merge -> client.merge(action.project, action.feature)
                        is Action.Delete -> client.delete(action.project, action.feature)
                        is Action.Restart ->
                            client.restart(action.project, action.scope, action.agent, action.force)
                    }
                    _finished.send(action)
                    ActionState.Idle
                } catch (e: CancellationException) {
                    throw e
                } catch (e: PmError.Refused) {
                    if (action is Action.Restart && e.code == MID_TURN && !action.force)
                        ActionState.Confirming(action.copy(force = true))
                    else ActionState.Failed(action, e.message.orEmpty())
                } catch (e: PmError.Forbidden) {
                    _grants.value = Grants.Unknown
                    refreshGrants()
                    ActionState.Failed(action, e.message.orEmpty())
                } catch (e: PmError.Unsupported) {
                    _grants.value = Grants.Unsupported
                    ActionState.Failed(action, UNSUPPORTED)
                } catch (e: PmError.Unreachable) {
                    ActionState.Failed(action, UNREACHABLE, Outcome.Lost)
                } catch (e: Exception) {
                    ActionState.Failed(action, e.message ?: e.javaClass.simpleName, Outcome.Broken)
                }
        }
    }

    companion object {
        const val MID_TURN = "mid-turn"
        const val UNSUPPORTED = "pm on the Mac is older than this app; upgrade it to do this here."
        const val UNREACHABLE =
            "pm serve stopped answering, so it isn't known whether this went through. " +
                "The app shows the outcome once it reconnects."
    }
}
