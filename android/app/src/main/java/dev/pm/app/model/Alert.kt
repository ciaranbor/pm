package dev.pm.app.model

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

/**
 * What an alert shows, kept in its notification so each repost builds on what is there: the push it
 * announces, a conversation with the agent the push names, and the permission prompt its Allow and
 * Deny answer. A push carries no detail (`commands/serve/push.rs`), so the agent's words come from
 * the server when the tailnet can be reached ([of]), and from the push alone until then ([bare]).
 */
@Serializable
data class Alert(
    val transition: PushedTransition,
    val lines: List<Line>,
    /** The permission prompt the alert's Allow and Deny answer; null for any other alert. */
    val prompt: Dialog? = null,
    /**
     * The user answered from the alert, which shows what was sent: nothing more is asked of them.
     */
    val settled: Boolean = false,
) {
    @Serializable
    data class Line(
        val text: String,
        val by: By = By.Agent,
        /** Why the user's line wasn't sent, when it wasn't. */
        val failure: String? = null,
    )

    enum class By {
        Agent,
        You,
        /** The app, saying what came of an answer. */
        Pm,
    }

    /** Whether typed text answers it: an agent blocked on the user, not a dialog. */
    val replyable: Boolean
        get() = transition.agent != null && transition.kindOf == AttentionKind.Blocked

    /** The alert after the user's reply `text` was sent. */
    fun replied(text: String): Alert =
        copy(lines = withoutFailures() + Line(text, By.You), settled = true)

    /** The alert after the user's `text` (a reply, or a choice's label) couldn't be sent. */
    fun failed(text: String, why: String): Alert =
        copy(lines = withoutFailures() + Line(text, By.You, why))

    /**
     * The alert after the prompt was answered with `said`: the next of the agent's `open` dialogs,
     * or, with none, the answer standing as the last word.
     */
    fun answered(said: String, open: List<Dialog>): Alert =
        next(copy(lines = withoutFailures() + Line(said, By.You)), open, settled = true)

    /** The alert after the prompt turned out to be closed, as `note` says. */
    fun closed(note: String, open: List<Dialog>): Alert =
        next(copy(lines = withoutFailures() + Line(note, By.Pm)), open, settled = false)

    private fun withoutFailures() = lines.dropLastWhile { it.failure != null }

    companion object {
        /** How much of a line an alert keeps; the shade shows less. */
        private const val LONGEST = 1000

        /** The alert of `transition` as its push alone tells it. */
        fun bare(transition: PushedTransition): Alert =
            Alert(
                transition,
                listOf(
                    Line(
                        when (transition.kindOf) {
                            AttentionKind.Blocked -> "Blocked on you"
                            AttentionKind.Asking -> "Waiting for your answer"
                            AttentionKind.Ready -> "Ready for review"
                            else -> transition.text
                        }
                    )
                ),
            )

        /**
         * The alert of `transition` with what the server says of it: the question or command an
         * asking agent's oldest open dialog (of `dialogs`, oldest first) puts, else what its state
         * says it waits on; a blocked feature's reason; a ready one's summary.
         */
        fun of(transition: PushedTransition, snapshot: Snapshot?, dialogs: List<Dialog>): Alert {
            val bare = bare(transition)
            val text =
                when (transition.kindOf) {
                    AttentionKind.Asking -> {
                        if (dialogs.isNotEmpty()) {
                            return next(bare.copy(lines = emptyList()), dialogs, settled = false)
                        }
                        snapshot
                            ?.agents(transition.project, transition.scope)
                            ?.find { it.name == transition.agent }
                            ?.waiting
                            ?.detail
                    }
                    AttentionKind.Blocked -> feature(transition, snapshot)?.blockedReason
                    AttentionKind.Ready ->
                        feature(transition, snapshot)?.summary?.let { "Ready for review: $it" }
                    else -> null
                }
            return text?.takeIf { it.isNotBlank() }?.let { bare.line(it) } ?: bare
        }

        private fun feature(transition: PushedTransition, snapshot: Snapshot?) =
            if (transition.scope == Snapshot.MAIN) null
            else snapshot?.feature(transition.project, transition.scope)

        private fun Alert.line(text: String) = copy(lines = listOf(Line(text.take(LONGEST))))

        /**
         * `alert` asking the first of `open`, with Allow and Deny when it is a permission prompt.
         */
        private fun next(alert: Alert, open: List<Dialog>, settled: Boolean): Alert {
            val dialog = open.firstOrNull() ?: return alert.copy(prompt = null, settled = settled)
            val more = open.size - 1
            val asked = if (more > 0) "${asked(dialog)} (+$more waiting)" else asked(dialog)
            val prompt = dialog.takeIf { it.kind == "permission" && it.groups?.negative != null }
            return alert.copy(
                lines = alert.lines + Line(asked.take(LONGEST)),
                prompt = prompt,
                settled = false,
            )
        }

        /** What `dialog` asks, in words: the question, or its title and target. */
        fun asked(dialog: Dialog): String =
            if (dialog.kind == "question") dialog.target ?: dialog.title
            else listOfNotNull(dialog.title, dialog.target).joinToString(" ")

        private val json = Json { ignoreUnknownKeys = true }

        fun parse(text: String): Alert? = runCatching {
            json.decodeFromString(serializer(), text)
        }
            .getOrNull()

        fun encode(alert: Alert): String = json.encodeToString(serializer(), alert)
    }
}
