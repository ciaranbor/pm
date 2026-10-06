package dev.pm.app.ui

import android.graphics.Typeface
import android.graphics.drawable.GradientDrawable
import android.os.Build
import android.text.InputType
import android.util.TypedValue
import android.view.Gravity
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputMethodManager
import android.widget.EditText
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.SegmentedButton
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.core.widget.TextViewCompat
import androidx.core.widget.doAfterTextChanged
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LifecycleEventEffect
import dev.pm.app.R

/** An edit of the notes: raw Markdown, or how it renders. Save and Discard sit in the top bar. */
@Composable
internal fun NotesEditor(
    model: NotesModel,
    state: NotesState.Editing,
    topBar: TopBarSlot,
    modifier: Modifier = Modifier,
) {
    var previewing by rememberSaveable { mutableStateOf(false) }
    var confirming by rememberSaveable { mutableStateOf(false) }
    val changed = state.changed
    val feedback = LocalFeedback.current
    LaunchedEffect(state.error) {
        val error = state.error ?: return@LaunchedEffect
        feedback.failed(error, if (error != NotesModel.TOO_LONG) model::save else null)
    }
    TopBarActions(topBar) {
        if (changed) {
            Box(
                Modifier.padding(horizontal = Spacing.s)
                    .size(8.dp)
                    .background(MaterialTheme.colorScheme.primary, CircleShape)
                    .semantics { contentDescription = "Unsaved changes" }
            )
        }
        IconButton(
            onClick = { if (changed) confirming = true else model.discard() },
            enabled = !state.saving,
        ) {
            Icon(painterResource(R.drawable.ic_close), if (changed) "Discard" else "Close")
        }
        PendingIconButton(
            painterResource(R.drawable.ic_check),
            "Save",
            onClick = model::save,
            pending = state.saving,
            enabled = changed || state.saving,
        )
    }
    Column(modifier.fillMaxSize().imePadding()) {
        SingleChoiceSegmentedButtonRow(
            Modifier.fillMaxWidth().padding(horizontal = Spacing.l, vertical = Spacing.s)
        ) {
            listOf("Edit", "Preview").forEachIndexed { i, label ->
                SegmentedButton(
                    selected = previewing == (i == 1),
                    onClick = { previewing = i == 1 },
                    shape = SegmentedButtonDefaults.itemShape(i, 2),
                ) {
                    Text(label)
                }
            }
        }
        if (previewing) {
            val text = remember { model.text.toString() }
            if (text.isBlank()) {
                EmptyState("Nothing to preview yet.")
            } else {
                MarkdownPage(text)
            }
        } else {
            Field(model, readOnly = state.saving)
        }
    }
    if (confirming) {
        AlertDialog(
            onDismissRequest = { confirming = false },
            title = { Text("Discard the edit?") },
            text = { Text("What you wrote here since the last save is lost.") },
            confirmButton = {
                TextButton(
                    onClick = {
                        confirming = false
                        model.discard()
                    }
                ) {
                    Text("Discard")
                }
            },
            dismissButton = {
                TextButton(onClick = { confirming = false }) { Text("Keep editing") }
            },
        )
    }
}

/**
 * The raw text, in a platform `EditText`: it paints only the lines on screen and scrolls itself,
 * where a Compose text field paints every line of the text on every frame, too slow for long notes.
 * Its own state saving is off, since the model keeps the text and a Bundle can't hold it.
 */
@Composable
private fun Field(model: NotesModel, readOnly: Boolean, modifier: Modifier = Modifier) {
    val colors = MaterialTheme.colorScheme
    val body = MaterialTheme.typography.bodyMedium
    val density = LocalDensity.current
    val host = LocalView.current
    val sidePx = with(density) { 16.dp.roundToPx() }
    var shown by remember { mutableStateOf<EditText?>(null) }

    fun leave(view: EditText) {
        view.layout?.let { model.top = it.getLineStart(it.getLineForVertical(view.scrollY)) }
        model.cursor = view.selectionStart.coerceAtLeast(0)
        model.keep()
    }
    DisposableEffect(host) {
        onDispose {
            // The keyboard would otherwise stay up, typing into nothing, until the text is tapped.
            host.context
                .getSystemService(InputMethodManager::class.java)
                ?.hideSoftInputFromWindow(host.windowToken, 0)
        }
    }
    LifecycleEventEffect(Lifecycle.Event.ON_STOP) { shown?.let(::leave) }

    AndroidView(
        factory = { context ->
            EditText(context).apply {
                tag = EDITOR_TAG
                isSaveEnabled = false
                background = null
                gravity = Gravity.TOP or Gravity.START
                typeface = Typeface.MONOSPACE
                inputType =
                    InputType.TYPE_CLASS_TEXT or
                        InputType.TYPE_TEXT_FLAG_MULTI_LINE or
                        InputType.TYPE_TEXT_FLAG_CAP_SENTENCES
                imeOptions = EditorInfo.IME_FLAG_NO_EXTRACT_UI
                isVerticalScrollBarEnabled = true
                setPadding(sidePx, 0, sidePx, sidePx)
                hint = "Markdown"
                setText(model.text)
                setSelection(model.cursor.coerceIn(0, length()))
                doAfterTextChanged { model.edited(it ?: "") }
                val top = model.top
                post {
                    val laid = layout ?: return@post
                    scrollTo(0, laid.getLineTop(laid.getLineForOffset(top.coerceIn(0, length()))))
                }
                shown = this
            }
        },
        update = { view ->
            view.setTextColor(colors.onSurface.toArgb())
            view.setHintTextColor(colors.onSurfaceVariant.toArgb())
            view.highlightColor = colors.primary.copy(alpha = 0.3f).toArgb()
            view.setTextSize(TypedValue.COMPLEX_UNIT_SP, body.fontSize.value)
            TextViewCompat.setLineHeight(view, TypedValue.COMPLEX_UNIT_SP, body.lineHeight.value)
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                val accent = colors.primary.toArgb()
                view.textCursorDrawable =
                    GradientDrawable().apply {
                        setColor(accent)
                        setSize(with(density) { 2.dp.roundToPx() }, 0)
                    }
                listOfNotNull(
                        view.textSelectHandle,
                        view.textSelectHandleLeft,
                        view.textSelectHandleRight,
                    )
                    .forEach { it.mutate().setTint(accent) }
            }
            view.isEnabled = !readOnly
        },
        onRelease = { view ->
            leave(view)
            shown = null
        },
        modifier = modifier.fillMaxSize(),
    )
}

/** The notes editor's field, for tests. */
internal const val EDITOR_TAG = "notes-editor"
