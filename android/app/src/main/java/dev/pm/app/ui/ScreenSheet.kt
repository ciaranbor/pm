package dev.pm.app.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.sizeIn
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilledIconButton
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.LifecycleStartEffect
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import dev.pm.app.R
import dev.pm.app.api.PmClient
import kotlinx.coroutines.launch

/**
 * The agent's terminal in a sheet, for a dialog the app can't show as a card: the bottom of its
 * screen, keys to press and a line of text to type. Its screen is read only while the sheet is open
 * and the app is in view.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun ScreenSheet(
    client: PmClient,
    project: String,
    scope: String,
    agent: String,
    dismiss: () -> Unit,
    model: ScreenModel = viewModel { ScreenModel(client, project, scope, agent) },
) {
    LifecycleStartEffect(model) {
        model.start()
        onStopOrDispose { model.stop() }
    }
    val screen by model.screen.collectAsStateWithLifecycle()
    val notice by model.notice.collectAsStateWithLifecycle()
    ModalBottomSheet(
        onDismissRequest = dismiss,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
    ) {
        ScreenPanel(screen, notice, press = model::press, type = model::type)
    }
}

/** A key on the sheet: tmux's name for it, what its button shows, and what it is called. */
private data class Key(val name: String, val label: String, val description: String = label)

private val Moves =
    listOf(
        Key("Escape", "Esc", "Escape"),
        Key("Tab", "Tab"),
        Key("BTab", "⇧Tab", "Shift Tab"),
        Key("Up", "↑", "Up"),
        Key("Down", "↓", "Down"),
        Key("Left", "←", "Left"),
        Key("Right", "→", "Right"),
        Key("Space", "Space"),
        Key("BSpace", "⌫", "Backspace"),
        Key("C-c", "Ctrl-C", "Control C"),
    )

private val Digits = ("123456789".toList() + '0').map { Key("$it", "$it") }

private val Enter = Key("Enter", "Enter ⏎", "Enter")

/**
 * The screen, scrolled to its bottom, over the keys and a line of text typed with nothing pressed
 * after it: Enter is its own key, since some dialogs filter as they are typed into.
 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
internal fun ScreenPanel(
    screen: String?,
    notice: String?,
    press: (String) -> Unit,
    type: suspend (String) -> Boolean,
    modifier: Modifier = Modifier,
) {
    var text by rememberSaveable { mutableStateOf("") }
    val scope = rememberCoroutineScope()
    val submit = {
        val typing = text
        if (typing.isNotEmpty()) scope.launch { if (type(typing) && text == typing) text = "" }
    }
    Column(
        modifier.fillMaxWidth().fillMaxHeight(0.9f).imePadding().padding(horizontal = 12.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Box(
            Modifier.weight(1f)
                .fillMaxWidth()
                .clip(MaterialTheme.shapes.small)
                .background(MaterialTheme.colorScheme.surfaceContainerHighest)
        ) {
            if (screen == null) {
                CircularProgressIndicator(Modifier.align(Alignment.Center))
            } else {
                TerminalView(screen, Modifier.fillMaxSize())
            }
        }
        if (notice != null) {
            Text(
                notice,
                color = MaterialTheme.colorScheme.error,
                style = MaterialTheme.typography.labelMedium,
            )
        }
        Keys(Moves + Enter, press)
        Keys(Digits, press)
        Row(verticalAlignment = Alignment.CenterVertically) {
            OutlinedTextField(
                value = text,
                onValueChange = { text = it.replace("\n", "") },
                singleLine = true,
                placeholder = { Text("Type at the terminal") },
                keyboardOptions = KeyboardOptions(imeAction = ImeAction.Send),
                keyboardActions = KeyboardActions(onSend = { submit() }),
                modifier = Modifier.weight(1f),
            )
            FilledIconButton(
                onClick = { submit() },
                enabled = text.isNotEmpty(),
                modifier = Modifier.padding(start = 4.dp),
            ) {
                Icon(painterResource(R.drawable.ic_send), "Type")
            }
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun Keys(keys: List<Key>, press: (String) -> Unit) {
    FlowRow(
        horizontalArrangement = Arrangement.spacedBy(4.dp),
        verticalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        keys.forEach { key ->
            val primary = key == Enter
            Surface(
                onClick = { press(key.name) },
                shape = MaterialTheme.shapes.small,
                color =
                    if (primary) MaterialTheme.colorScheme.primary
                    else MaterialTheme.colorScheme.secondaryContainer,
                modifier =
                    Modifier.sizeIn(minWidth = 40.dp, minHeight = 40.dp).semantics {
                        contentDescription = key.description
                    },
            ) {
                Box(contentAlignment = Alignment.Center) {
                    Text(
                        key.label,
                        style = MaterialTheme.typography.labelLarge,
                        modifier = Modifier.padding(horizontal = 10.dp, vertical = 8.dp),
                    )
                }
            }
        }
    }
}
