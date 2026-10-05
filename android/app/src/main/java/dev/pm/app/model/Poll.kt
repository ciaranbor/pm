package dev.pm.app.model

/**
 * What a poll of the snapshot alerts, for a phone no push reaches: the alerts `pm serve` would have
 * pushed since the last poll, judged by the alerts already made, as a push announces them. An alert
 * is made once per episode: while its condition holds ([PushedTransition.holds]) it is not made
 * again. A ready feature's alert waits until its team is quiet, as the server's does.
 *
 * Coarser than the server's rule (`attention/transition.rs`), which sees every change: a poll sees
 * only what holds as it reads, so an episode that begins and ends between two polls goes unseen.
 */
object Poll {
    /**
     * The alerts to keep for the next poll and those to make now. With no earlier poll (`alerted`
     * null), what stands is taken as already alerted but an agent asking, as the server alerts a
     * dialog up as it starts.
     */
    fun judge(
        alerted: Set<PushedTransition>?,
        snapshot: Snapshot,
    ): Pair<Set<PushedTransition>, List<PushedTransition>> {
        val standing = standing(snapshot)
        if (alerted == null) {
            val asking = standing.filter { it.kindOf == AttentionKind.Asking }
            return standing.map { it.key }.toSet() to asking
        }
        val kept = alerted.filter { it.holds(snapshot) }.toMutableSet()
        val made = standing.filter { kept.add(it.key) }
        return kept to made
    }

    /** Every alert `snapshot` shows grounds for now. */
    private fun standing(snapshot: Snapshot): List<PushedTransition> {
        fun agents(project: String, scope: String, agents: List<AgentSnapshot>) =
            agents.mapNotNull { agent ->
                when (agent.stateOf) {
                    AgentState.Asking -> PushedTransition(project, scope, "asking", agent.name)
                    AgentState.Dead -> PushedTransition(project, scope, "dead", agent.name)
                    else -> null
                }
            }
        val mains =
            snapshot.projects.flatMap { p ->
                agents(p.name, Snapshot.MAIN, p.main?.agents.orEmpty())
            }
        val features =
            snapshot.features.flatMap { f ->
                val named = f.attention.agent
                listOfNotNull(
                    PushedTransition(f.project, f.name, "blocked", named).takeIf {
                        f.progress == "blocked"
                    },
                    PushedTransition(f.project, f.name, "ready", named).takeIf {
                        !f.working && (f.progress == "ready" || f.lifecycle == "approved")
                    },
                ) + agents(f.project, f.name, f.agents)
            }
        return mains + features
    }
}
