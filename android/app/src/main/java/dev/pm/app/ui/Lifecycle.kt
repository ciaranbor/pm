package dev.pm.app.ui

import dev.pm.app.api.PmClient
import dev.pm.app.api.PmError
import dev.pm.app.model.Snapshot
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.receiveAsFlow
import kotlinx.coroutines.launch

/** What the app can ask `pm serve` to do to a project, a feature or an agent. */
sealed interface Action {
    val project: String

    /** What it acts on: the project's, the feature's or the agent's name. */
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

    data class OpenProject(override val project: String) : Action {
        override val subject = project
        override val verb = "Open"
    }

    /** `working`: how many of its agents are busy, asking, or waiting on background work. */
    data class CloseProject(override val project: String, val working: Int) : Action {
        override val subject = project
        override val verb = "Close"
    }

    data class DeleteProject(override val project: String) : Action {
        override val subject = project
        override val verb = "Delete"
    }
}

/**
 * The scope an action removes once it goes through, as project and scope; `main` stands for the
 * whole project. Null for one that removes nothing.
 */
val Action.removes: Pair<String, String>?
    get() =
        when (this) {
            is Action.Merge -> project to feature
            is Action.Delete -> project to feature
            is Action.DeleteProject -> project to Snapshot.MAIN
            is Action.Restart,
            is Action.OpenProject,
            is Action.CloseProject -> null
        }

/** Where an action stands. */
sealed interface ActionState {
    data object Idle : ActionState

    /** Asked to confirm a merge, a delete, a close, or a restart that would interrupt a turn. */
    data class Confirming(val action: Action) : ActionState

    /** On its way; `confirmed` if it was confirmed first, so its confirmation shows it going. */
    data class Running(val action: Action, val confirmed: Boolean = false) : ActionState

    /** It went through, but with `warnings`, in the server's words. */
    data class Warned(val action: Action, val warnings: List<String>) : ActionState

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

/**
 * Lifecycle actions: confirmed where they destroy or interrupt, run one at a time, with their
 * refusals kept as the server words them. Each that went through is sent on [finished].
 */
class Lifecycle(private val scope: CoroutineScope, private val client: () -> PmClient?) {
    private val _state = MutableStateFlow<ActionState>(ActionState.Idle)
    val state: StateFlow<ActionState> = _state.asStateFlow()

    private val _finished = Channel<Action>(Channel.BUFFERED)

    /** Each action that went through, once. */
    val finished: Flow<Action> = _finished.receiveAsFlow()

    /** Start `action` from its menu: a restart or an open runs at once, the rest ask first. */
    fun ask(action: Action) {
        if (_state.value is ActionState.Running) return
        when (action) {
            is Action.Restart,
            is Action.OpenProject -> run(action)
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
        _state.value = ActionState.Running(action, _state.value is ActionState.Confirming)
        scope.launch {
            _state.value =
                try {
                    val warnings =
                        when (action) {
                            is Action.Merge -> client.merge(action.project, action.feature)
                            is Action.Delete -> client.delete(action.project, action.feature)
                            is Action.Restart -> {
                                client.restart(
                                    action.project,
                                    action.scope,
                                    action.agent,
                                    action.force,
                                )
                                emptyList()
                            }
                            is Action.OpenProject -> client.openProject(action.project)
                            is Action.CloseProject -> {
                                client.closeProject(action.project)
                                emptyList()
                            }
                            is Action.DeleteProject -> client.deleteProject(action.project)
                        }
                    _finished.send(action)
                    if (warnings.isEmpty()) ActionState.Idle
                    else ActionState.Warned(action, warnings)
                } catch (e: CancellationException) {
                    throw e
                } catch (e: PmError.Refused) {
                    if (action is Action.Restart && e.code == MID_TURN && !action.force)
                        ActionState.Confirming(action.copy(force = true))
                    else ActionState.Failed(action, e.message.orEmpty())
                } catch (e: PmError.Unsupported) {
                    ActionState.Failed(action, e.advice("do this here"))
                } catch (e: PmError.Unreachable) {
                    ActionState.Failed(action, UNREACHABLE, Outcome.Lost)
                } catch (e: Exception) {
                    ActionState.Failed(action, e.message ?: e.javaClass.simpleName, Outcome.Broken)
                }
        }
    }

    companion object {
        const val MID_TURN = "mid-turn"
        const val UNREACHABLE =
            "pm serve stopped answering, so it isn't known whether this went through. " +
                "The app shows the outcome once it reconnects."
    }
}
