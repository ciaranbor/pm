package dev.pm.app.ui

import android.content.ClipData
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.platform.ClipEntry
import androidx.compose.ui.platform.LocalClipboard
import androidx.compose.ui.platform.LocalSoftwareKeyboardController
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.style.TextOverflow
import androidx.lifecycle.compose.LifecycleStartEffect
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import dev.pm.app.R
import dev.pm.app.api.PmClient
import kotlin.time.Duration.Companion.seconds
import kotlinx.coroutines.delay
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
    model: ScreenModel =
        viewModel(key = "screen/$agent") { ScreenModel(client, project, scope, agent) },
) {
    LifecycleStartEffect(model) {
        model.start()
        onStopOrDispose { model.stop() }
    }
    val screen by model.screen.collectAsStateWithLifecycle()
    val notice by model.notice.collectAsStateWithLifecycle()
    val pressing by model.pressing.collectAsStateWithLifecycle()
    ModalBottomSheet(
        onDismissRequest = dismiss,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
    ) {
        ScreenPanel(screen, notice, pressing, press = model::press, type = model::type)
    }
}

/**
 * The screen, scrolled to its bottom, under copy actions and over one row of keys and a line of
 * text typed with nothing pressed after it: Enter is its own key, since some dialogs filter as they
 * are typed into. While Control is held, a letter typed is pressed with it instead.
 */
@Composable
internal fun ScreenPanel(
    screen: String?,
    notice: String?,
    pressing: List<String>,
    press: (String) -> Unit,
    type: suspend (String) -> Boolean,
    modifier: Modifier = Modifier,
) {
    var text by rememberSaveable { mutableStateOf("") }
    var typing by remember { mutableStateOf(false) }
    var digits by rememberSaveable { mutableStateOf(false) }
    var ctrl by rememberSaveable { mutableStateOf(Ctrl.Off) }
    val field = remember { FocusRequester() }
    val keyboard = LocalSoftwareKeyboardController.current
    val scope = rememberCoroutineScope()
    val submit = {
        val typed = text
        if (typed.isNotEmpty() && !typing) {
            typing = true
            scope.launch {
                try {
                    if (type(typed) && text == typed) text = ""
                } finally {
                    typing = false
                }
            }
        }
    }
    Column(
        modifier.fillMaxWidth().fillMaxHeight(0.9f).imePadding().padding(horizontal = Spacing.m),
        verticalArrangement = Arrangement.spacedBy(Spacing.s),
    ) {
        val rows = remember(screen) { screen?.let(::screenRows).orEmpty() }
        CopyBar(rows)
        Box(
            Modifier.weight(1f)
                .fillMaxWidth()
                .clip(MaterialTheme.shapes.small)
                .background(MaterialTheme.colorScheme.surfaceContainerHighest)
        ) {
            if (screen == null) {
                CircularProgressIndicator(Modifier.align(Alignment.Center))
            } else {
                TerminalView(rows, Modifier.fillMaxSize())
            }
        }
        if (notice != null) {
            Text(
                notice,
                color = MaterialTheme.colorScheme.error,
                style = MaterialTheme.typography.labelMedium,
            )
        }
        KeyRow(
            digits = digits,
            toggleDigits = { digits = !digits },
            ctrl = ctrl,
            setCtrl = {
                ctrl = it
                if (it != Ctrl.Off) {
                    field.requestFocus()
                    keyboard?.show()
                }
            },
            pressing = pressing,
            press = { key ->
                press(ctrl.on(key.name))
                ctrl = ctrl.used()
            },
        )
        Row(verticalAlignment = Alignment.CenterVertically) {
            OutlinedTextField(
                value = text,
                onValueChange = { new ->
                    val clean = new.replace("\n", "")
                    val letter =
                        clean.getOrNull(clean.commonPrefixWith(text).length)?.takeIf {
                            ctrl != Ctrl.Off &&
                                clean.length > text.length &&
                                it.lowercaseChar() in 'a'..'z'
                        }
                    if (letter != null) {
                        press(withCtrl(letter))
                        ctrl = ctrl.used()
                    } else {
                        text = clean
                    }
                },
                singleLine = true,
                placeholder = {
                    Text(if (ctrl == Ctrl.Off) "Type at the terminal" else "Ctrl + a letter")
                },
                keyboardOptions = KeyboardOptions(imeAction = ImeAction.Send),
                keyboardActions = KeyboardActions(onSend = { submit() }),
                modifier = Modifier.weight(1f).focusRequester(field),
            )
            PendingIconButton(
                painterResource(R.drawable.ic_send),
                "Type",
                onClick = { submit() },
                pending = typing,
                enabled = typing || text.isNotEmpty(),
                filled = true,
                modifier = Modifier.padding(start = Spacing.xs),
            )
        }
    }
}

/**
 * Copy the screen, or a link on it whole however the pane wrapped it; with several links, a menu of
 * them. A copy shows a tick on its button for a moment.
 */
@Composable
private fun CopyBar(rows: List<String>) {
    val clipboard = LocalClipboard.current
    val scope = rememberCoroutineScope()
    val links = remember(rows) { screenLinks(rows).distinct() }
    var copied by remember { mutableStateOf<String?>(null) }
    var choosing by remember { mutableStateOf(false) }
    LaunchedEffect(copied) {
        if (copied != null) {
            delay(2.seconds)
            copied = null
        }
    }
    fun copy(what: String, text: String) {
        scope.launch {
            clipboard.setClipEntry(ClipEntry(ClipData.newPlainText(what, text)))
            copied = what
        }
    }
    Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
        Text(
            "Terminal",
            style = MaterialTheme.typography.titleMedium,
            modifier = Modifier.weight(1f),
        )
        if (links.isNotEmpty()) {
            Box {
                IconButton(
                    onClick = {
                        if (links.size == 1) copy(LINK, links.single()) else choosing = true
                    }
                ) {
                    CopyIcon(copied == LINK, R.drawable.ic_link, "Copy link")
                }
                DropdownMenu(expanded = choosing, onDismissRequest = { choosing = false }) {
                    links.forEach { link ->
                        DropdownMenuItem(
                            text = {
                                Text(link, maxLines = 1, overflow = TextOverflow.MiddleEllipsis)
                            },
                            onClick = {
                                choosing = false
                                copy(LINK, link)
                            },
                        )
                    }
                }
            }
        }
        IconButton(
            onClick = { copy(SCREEN, rows.joinToString("\n")) },
            enabled = rows.isNotEmpty(),
        ) {
            CopyIcon(copied == SCREEN, R.drawable.ic_content_copy, "Copy screen")
        }
    }
}

@Composable
private fun CopyIcon(done: Boolean, icon: Int, description: String) {
    Icon(
        painterResource(if (done) R.drawable.ic_check else icon),
        contentDescription = if (done) "Copied" else description,
    )
}

private const val LINK = "Link"
private const val SCREEN = "Screen"
