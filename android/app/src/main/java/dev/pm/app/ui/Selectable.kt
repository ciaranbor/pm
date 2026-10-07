package dev.pm.app.ui

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.text.selection.SelectionState
import androidx.compose.foundation.text.selection.rememberSelectionState
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier

/** Text the user can select; while some is, Back clears the selection and nothing else. */
@Composable
fun Selectable(
    modifier: Modifier = Modifier,
    state: SelectionState = rememberSelectionState(),
    content: @Composable () -> Unit,
) {
    BackHandler(enabled = state.selectedTexts.any { it.isNotEmpty() }) { state.clear() }
    SelectionContainer(state, modifier, content)
}
