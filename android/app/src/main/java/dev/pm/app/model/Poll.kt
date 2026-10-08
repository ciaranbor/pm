package dev.pm.app.model

/**
 * What a poll of the snapshot alerts, for a phone no push reaches: the alerts `pm serve` would have
 * pushed since the last poll, by the server's rule (`attention/transition.rs`). A scope alerts as
 * its attention, the top-ranked kind it needs, enters blocked, asking or ready (a `main` only
 * asking), once per episode of that kind: while the kind's condition holds
 * ([PushedTransition.holds]) it does not alert again, even after something outranking it passes. A
 * kind outranked as it begins alerts later, once it is the scope's attention. A busy agent holds a
 * ready feature's attention back, and so its alert. An agent alerts as it dies.
 *
 * Coarser than the server, which sees every change: a poll sees only what holds as it reads, so an
 * episode that begins and ends between two polls goes unseen.
 */
object Poll {
    /**
     * The alerts to keep for the next poll and those to make now. With no earlier poll (`alerted`
     * null), a standing blocked, ready or dead agent is taken as already alerted, as the server
     * takes a scope it judges first, while an agent asking still alerts.
     */
    fun judge(
        alerted: Set<PushedTransition>?,
        snapshot: Snapshot,
    ): Pair<Set<PushedTransition>, List<PushedTransition>> {
        val first = alerted == null
        val kept = alerted.orEmpty().filter { it.holds(snapshot) }.toMutableSet()
        val made = mutableListOf<PushedTransition>()

        fun judge(
            project: String,
            scope: String,
            attention: Attention,
            agents: List<AgentSnapshot>,
            alerting: Set<AttentionKind>,
        ) {
            if (first) {
                for (kind in STANDING intersect alerting) {
                    val episode = PushedTransition(project, scope, kind.wire)
                    if (episode.holds(snapshot)) kept += episode
                }
            }
            val kind = attention.kindOf
            if (kind in alerting) {
                // Asking is one episode per scope, as the server judges it, whichever agent asks.
                val episode = PushedTransition(project, scope, kind.wire)
                if (kept.add(episode)) made += episode.copy(agent = attention.agent)
            }
            for (agent in agents.filter { it.stateOf == AgentState.Dead }) {
                val died = PushedTransition(project, scope, AttentionKind.Dead.wire, agent.name)
                if (kept.add(died) && !first) made += died
            }
        }

        for (p in snapshot.projects) {
            val main = p.main ?: continue
            judge(p.name, Snapshot.MAIN, main.attention, main.agents, MAIN_ALERTING)
        }
        for (f in snapshot.features) {
            judge(f.project, f.name, f.attention, f.agents, ALERTING)
        }
        return kept to made
    }

    private val STANDING = setOf(AttentionKind.Blocked, AttentionKind.Ready)
    private val ALERTING = STANDING + AttentionKind.Asking
    private val MAIN_ALERTING = setOf(AttentionKind.Blocked, AttentionKind.Asking)
}
