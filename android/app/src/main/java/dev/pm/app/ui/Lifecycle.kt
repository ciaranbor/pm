package dev.pm.app.ui

import dev.pm.app.api.PmClient
import dev.pm.app.api.PmError
import dev.pm.app.api.reason
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

    /**
     * `pm agent spawn`: adds `agent` to the scope, or, registered already (`existing`), starts it
     * or sends it the context. `definition` null launches the agent's own.
     */
    data class Spawn(
        override val project: String,
        val scope: String,
        val agent: String,
        val definition: String? = null,
        val context: String? = null,
        val existing: Boolean = false,
    ) : Action {
        override val subject = agent
        override val verb = "Add"
    }

    /** A spawn of a stopped or dead agent with nothing else: it starts on its conversation. */
    data class Start(override val project: String, val scope: String, val agent: String) : Action {
        override val subject = agent
        override val verb = "Start"
    }

    /** `working`: whether the agent is busy, asking, or waiting on background work. */
    data class Stop(
        override val project: String,
        val scope: String,
        val agent: String,
        val working: Boolean = false,
    ) : Action {
        override val subject = agent
        override val verb = "Stop"
    }

    data class DeleteAgent(override val project: String, val scope: String, val agent: String) :
        Action {
        override val subject = agent
        override val verb = "Delete"
    }

    /** A new agent `name` starting on a copy of `source`'s conversation. */
    data class Fork(
        override val project: String,
        val scope: String,
        val source: String,
        val name: String,
    ) : Action {
        override val subject = source
        override val verb = "Fork"
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
            is Action.Spawn,
            is Action.Start,
            is Action.Stop,
            is Action.DeleteAgent,
            is Action.Fork,
            is Action.OpenProject,
            is Action.CloseProject -> null
        }

/** The agent an action that went through puts on screen, as project, scope and agent. */
val Action.shows: Triple<String, String, String>?
    get() =
        when (this) {
            is Action.Spawn -> Triple(project, scope, agent)
            is Action.Start -> Triple(project, scope, agent)
            is Action.Fork -> Triple(project, scope, name)
            else -> null
        }

/** An action that went through, and what it did in words for the snackbar. */
data class Finished(val action: Action, val said: String = action.done)

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

    private val _finished = Channel<Finished>(Channel.BUFFERED)

    /** Each action that went through, once. */
    val finished: Flow<Finished> = _finished.receiveAsFlow()

    /**
     * Start `action` from its menu or form: a restart, an open, and what a form asked for run at
     * once; the rest ask first.
     */
    fun ask(action: Action) {
        if (_state.value is ActionState.Running) return
        when (action) {
            is Action.Restart,
            is Action.Spawn,
            is Action.Start,
            is Action.Fork,
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
                    var said = action.done
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
                            is Action.Spawn -> {
                                val outcome =
                                    client.spawn(
                                        action.project,
                                        action.scope,
                                        action.agent,
                                        action.definition,
                                        action.context,
                                    )
                                val sent =
                                    if (action.context != null) "; sent it the context" else ""
                                said =
                                    when (outcome) {
                                        ALREADY_ACTIVE -> "${action.agent} was already running$sent"
                                        else ->
                                            if (action.existing) "Started ${action.agent}$sent"
                                            else said
                                    }
                                emptyList()
                            }
                            is Action.Start -> {
                                client.spawn(action.project, action.scope, action.agent)
                                emptyList()
                            }
                            is Action.Stop -> {
                                client.stop(action.project, action.scope, action.agent)
                                emptyList()
                            }
                            is Action.DeleteAgent -> {
                                client.deleteAgent(action.project, action.scope, action.agent)
                                emptyList()
                            }
                            is Action.Fork -> {
                                client.fork(
                                    action.project,
                                    action.scope,
                                    action.source,
                                    action.name,
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
                    _finished.send(Finished(action, said))
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
                    ActionState.Failed(action, e.reason(), Outcome.Broken)
                }
        }
    }

    companion object {
        const val MID_TURN = "mid-turn"
        const val ALREADY_ACTIVE = "already-active"
        const val UNREACHABLE =
            "pm serve stopped answering, so it isn't known whether this went through. " +
                "The app shows the outcome once it reconnects."
    }
}
