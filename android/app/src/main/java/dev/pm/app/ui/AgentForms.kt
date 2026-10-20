package dev.pm.app.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import dev.pm.app.api.PmClient
import dev.pm.app.api.PmError
import dev.pm.app.model.AgentDefinition
import dev.pm.app.model.isAgentName

/**
 * Add an agent to `scope`: a definition from those the server lists, a name, which defaults to the
 * definition's, and an optional context. A name in `existing` is that agent: the spawn starts it on
 * its own definition, or sends it the context, so the form says so rather than refusing it.
 */
@Composable
fun AddAgentDialog(
    client: PmClient,
    project: String,
    scope: String,
    existing: List<String>,
    submit: (Action.Spawn) -> Unit,
    dismiss: () -> Unit,
    model: ReadModel<List<AgentDefinition>?> =
        viewModel(key = "definitions/$project") {
            ReadModel(client) {
                try {
                    definitions(project)
                } catch (_: PmError.Unsupported) {
                    null
                }
            }
        },
) {
    val read by model.uiState.collectAsStateWithLifecycle()
    AddAgentForm(read, model::retry, project, scope, existing, submit, dismiss)
}

/**
 * [AddAgentDialog] showing `read`, the server's definitions as far as they are read; null for a
 * server that predates them.
 */
@Composable
internal fun AddAgentForm(
    read: ReadState<List<AgentDefinition>?>,
    retry: () -> Unit,
    project: String,
    scope: String,
    existing: List<String>,
    submit: (Action.Spawn) -> Unit,
    dismiss: () -> Unit,
) {
    val definitions = (read as? ReadState.Shown)?.value.orEmpty()
    var chosen by rememberSaveable { mutableStateOf<String?>(null) }
    var name by rememberSaveable { mutableStateOf("") }
    var named by rememberSaveable { mutableStateOf(false) }
    var context by rememberSaveable { mutableStateOf("") }
    LaunchedEffect(definitions) {
        if (chosen == null) definitions.firstOrNull { it.error == null }?.let { chosen = it.name }
    }
    val definition = chosen
    val shownName = if (named) name else definition.orEmpty()
    val valid = isAgentName(shownName)
    AlertDialog(
        onDismissRequest = dismiss,
        title = { Text("Add an agent to $scope") },
        text = {
            Column(
                Modifier.verticalScroll(rememberScrollState()),
                verticalArrangement = Arrangement.spacedBy(Spacing.m),
            ) {
                when (read) {
                    ReadState.Loading ->
                        CircularProgressIndicator(Modifier.align(Alignment.CenterHorizontally))
                    is ReadState.Shown ->
                        read.value?.let { Definitions(it, chosen) { chosen = it } }
                            ?: Text(
                                PmError.Unsupported().advice("add agents here"),
                                color = MaterialTheme.colorScheme.error,
                            )
                    else ->
                        Column(verticalArrangement = Arrangement.spacedBy(Spacing.xs)) {
                            Text(
                                when (read) {
                                    ReadState.Unreachable -> "pm serve unreachable"
                                    is ReadState.Failed -> read.reason
                                    else -> "This server lists no definitions."
                                },
                                color = MaterialTheme.colorScheme.error,
                            )
                            TextButton(onClick = retry) { Text("Retry") }
                        }
                }
                OutlinedTextField(
                    shownName,
                    {
                        name = it
                        named = true
                    },
                    label = { Text("Name") },
                    singleLine = true,
                    isError = shownName.isNotEmpty() && !valid,
                    supportingText =
                        when {
                            shownName.isNotEmpty() && !valid -> {
                                { Text("Letters, digits, - and _ only") }
                            }
                            shownName in existing -> {
                                {
                                    Text(
                                        "$shownName exists: this starts it if it's stopped, " +
                                            "on its own definition, and sends it the context"
                                    )
                                }
                            }
                            else -> null
                        },
                    modifier = Modifier.fillMaxWidth(),
                )
                OutlinedTextField(
                    context,
                    { context = it },
                    label = { Text("Context (optional)") },
                    minLines = 3,
                    modifier = Modifier.fillMaxWidth(),
                )
            }
        },
        confirmButton = {
            TextButton(
                onClick = {
                    submit(
                        Action.Spawn(
                            project,
                            scope,
                            shownName,
                            definition?.takeIf { it != shownName && shownName !in existing },
                            context.takeIf { it.isNotBlank() },
                            existing = shownName in existing,
                        )
                    )
                },
                enabled = definition != null && valid,
            ) {
                Text("Add")
            }
        },
        dismissButton = { TextButton(onClick = dismiss) { Text("Cancel") } },
    )
}

/**
 * The definitions to pick from, each with what a spawn of it launches; read-only. One config can't
 * resolve can't be picked: its spawn would fail.
 */
@Composable
private fun Definitions(
    definitions: List<AgentDefinition>,
    chosen: String?,
    choose: (String) -> Unit,
) {
    Column(
        Modifier.heightIn(max = 280.dp).verticalScroll(rememberScrollState()).selectableGroup()
    ) {
        definitions.forEach { d ->
            Row(
                Modifier.fillMaxWidth()
                    .selectable(
                        d.name == chosen,
                        enabled = d.error == null,
                        role = Role.RadioButton,
                    ) {
                        choose(d.name)
                    }
                    .padding(vertical = Spacing.xs),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(Spacing.s),
            ) {
                RadioButton(selected = d.name == chosen, onClick = null, enabled = d.error == null)
                Column {
                    Text(d.name, style = MaterialTheme.typography.titleSmall)
                    d.description?.let { Text(it, style = MaterialTheme.typography.bodySmall) }
                    if (d.launches.isNotEmpty()) {
                        Text(
                            d.launches,
                            style = MaterialTheme.typography.bodySmall,
                            color =
                                if (d.error != null) MaterialTheme.colorScheme.error
                                else MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
            }
        }
    }
}

/** Fork `source` as a new agent, named here; a name in `existing` is taken. */
@Composable
fun ForkDialog(
    project: String,
    scope: String,
    source: String,
    existing: List<String>,
    submit: (Action.Fork) -> Unit,
    dismiss: () -> Unit,
) {
    var name by rememberSaveable { mutableStateOf(forkName(source, existing)) }
    val error =
        when {
            !isAgentName(name) -> "Letters, digits, - and _ only"
            name in existing -> "$name exists"
            else -> null
        }
    AlertDialog(
        onDismissRequest = dismiss,
        title = { Text("Fork $source") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(Spacing.m)) {
                Text("A new agent that starts on a copy of $source's conversation.")
                OutlinedTextField(
                    name,
                    { name = it },
                    label = { Text("New agent's name") },
                    singleLine = true,
                    isError = error != null && name.isNotEmpty(),
                    supportingText = error?.takeIf { name.isNotEmpty() }?.let { { Text(it) } },
                    modifier = Modifier.fillMaxWidth(),
                )
            }
        },
        confirmButton = {
            TextButton(
                onClick = { submit(Action.Fork(project, scope, source, name)) },
                enabled = error == null,
            ) {
                Text("Fork")
            }
        },
        dismissButton = { TextButton(onClick = dismiss) { Text("Cancel") } },
    )
}

/** `<source>-fork`, numbered past any name in `existing`. */
internal fun forkName(source: String, existing: List<String>): String =
    generateSequence(1) { it + 1 }
        .map { if (it == 1) "$source-fork" else "$source-fork-$it" }
        .first { it !in existing }
