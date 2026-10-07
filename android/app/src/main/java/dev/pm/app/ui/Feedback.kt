package dev.pm.app.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FilledIconButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LocalContentColor
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Snackbar
import androidx.compose.material3.SnackbarDuration
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.SnackbarResult
import androidx.compose.material3.SnackbarVisuals
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.painter.Painter
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import dev.pm.app.R
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch

/** How loud a button is: filled for the one main action, outlined or text for the rest. */
enum class Emphasis {
    Filled,
    Outlined,
    Text,
}

/** What a pending button tells TalkBack while its action runs. */
private const val PENDING = "In progress"

/**
 * While `pending`, the button's `label` and that it is in progress: the label is hidden then, and
 * accessibility skips what is drawn transparent.
 */
private fun Modifier.pendingSemantics(pending: Boolean, label: String) = semantics {
    if (pending) {
        contentDescription = label
        stateDescription = PENDING
    }
}

/**
 * A button for a network action. While `pending` it shows progress in place of its label, at the
 * same size, and ignores taps; the screen disables the buttons beside it (`enabled`) meanwhile, so
 * one action runs at a time.
 */
@Composable
fun PendingButton(
    text: String,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    pending: Boolean = false,
    enabled: Boolean = true,
    emphasis: Emphasis = Emphasis.Filled,
    /** The label's and spinner's colour; the emphasis's own where unspecified. */
    color: Color = Color.Unspecified,
) {
    val click = { if (!pending) onClick() }
    val shown = modifier.pendingSemantics(pending, text)
    val content: @Composable () -> Unit = {
        Box(contentAlignment = Alignment.Center) {
            Text(text, Modifier.alpha(if (pending) 0f else 1f))
            if (pending) {
                CircularProgressIndicator(
                    Modifier.size(18.dp),
                    color = LocalContentColor.current,
                    strokeWidth = 2.dp,
                )
            }
        }
    }
    when (emphasis) {
        Emphasis.Filled ->
            Button(
                click,
                shown,
                enabled = enabled,
                colors = ButtonDefaults.buttonColors(contentColor = color),
            ) {
                content()
            }
        Emphasis.Outlined ->
            OutlinedButton(
                click,
                shown,
                enabled = enabled,
                colors = ButtonDefaults.outlinedButtonColors(contentColor = color),
            ) {
                content()
            }
        Emphasis.Text ->
            TextButton(
                click,
                shown,
                enabled = enabled,
                colors = ButtonDefaults.textButtonColors(contentColor = color),
            ) {
                content()
            }
    }
}

/** An icon button for a network action, pending as a [PendingButton] is. */
@Composable
fun PendingIconButton(
    icon: Painter,
    description: String,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    pending: Boolean = false,
    enabled: Boolean = true,
    filled: Boolean = false,
) {
    val click = { if (!pending) onClick() }
    val shown = modifier.pendingSemantics(pending, description)
    val content: @Composable () -> Unit = {
        if (pending) {
            CircularProgressIndicator(
                Modifier.size(20.dp),
                color = LocalContentColor.current,
                strokeWidth = 2.dp,
            )
        } else Icon(icon, description)
    }
    if (filled) FilledIconButton(click, shown, enabled = enabled) { content() }
    else IconButton(click, shown, enabled = enabled) { content() }
}

/** A snackbar's message, and whether it reports a failure. */
private data class Report(
    override val message: String,
    override val actionLabel: String?,
    val error: Boolean,
) : SnackbarVisuals {
    /** A failure that offers Retry waits to be retried or dismissed. */
    override val withDismissAction = error && actionLabel != null
    override val duration =
        when {
            withDismissAction -> SnackbarDuration.Indefinite
            actionLabel != null -> SnackbarDuration.Long
            else -> SnackbarDuration.Short
        }
}

/**
 * The app's one place for the outcome of an action: a snackbar saying it was done, or that it
 * failed, with Retry where trying again makes sense. The latest outcome replaces the one showing.
 */
@Stable
class Feedback(val host: SnackbarHostState, private val scope: CoroutineScope) {
    /** How far above the bottom a page's own controls reach, for the snackbar to sit above. */
    var lift by mutableStateOf(0.dp)

    /** Report `message`, with `action` offering a next step where there is one. */
    fun done(message: String, action: String? = null, onAction: () -> Unit = {}) =
        show(message, action, error = false, onAction)

    fun failed(message: String, retry: (() -> Unit)? = null) =
        show(message, retry?.let { "Retry" }, error = true) { retry?.invoke() }

    private fun show(message: String, action: String?, error: Boolean, onAction: () -> Unit) {
        scope.launch {
            host.currentSnackbarData?.dismiss()
            val result = host.showSnackbar(Report(message, action, error))
            if (result == SnackbarResult.ActionPerformed) onAction()
        }
    }
}

// The app's snackbar, provided by App as MaterialTheme provides its tokens.
@Suppress("ComposeCompositionLocalUsage")
val LocalFeedback = staticCompositionLocalOf<Feedback> { error("LocalFeedback not provided") }

/** Where [Feedback] shows: failures in the error colours. */
@Composable
fun FeedbackHost(feedback: Feedback, modifier: Modifier = Modifier) {
    SnackbarHost(feedback.host, modifier.padding(bottom = feedback.lift)) { data ->
        if ((data.visuals as? Report)?.error == true) {
            Snackbar(
                data,
                containerColor = MaterialTheme.colorScheme.errorContainer,
                contentColor = MaterialTheme.colorScheme.onErrorContainer,
                actionColor = MaterialTheme.colorScheme.onErrorContainer,
            )
        } else Snackbar(data)
    }
}

@Composable
fun Centered(modifier: Modifier = Modifier, content: @Composable () -> Unit) {
    Box(modifier.fillMaxSize().padding(Spacing.xl), contentAlignment = Alignment.Center) {
        content()
    }
}

/** A screen with nothing to show: why, and the one thing to do about it, if any. */
@Composable
fun EmptyState(
    title: String,
    modifier: Modifier = Modifier,
    hint: String? = null,
    action: String? = null,
    onAction: () -> Unit = {},
) {
    Message(title, hint, action, onAction, modifier)
}

/**
 * A screen that couldn't show what it should: what went wrong, and what to do about it, `pending`
 * while that runs; no action where the app retries by itself.
 */
@Composable
fun ErrorState(
    title: String,
    onAction: (() -> Unit)?,
    modifier: Modifier = Modifier,
    hint: String? = null,
    action: String = "Retry",
    pending: Boolean = false,
) {
    Message(title, hint, action.takeIf { onAction != null }, onAction ?: {}, modifier, pending) {
        Icon(
            painterResource(R.drawable.ic_error),
            null,
            Modifier.size(32.dp),
            tint = MaterialTheme.colorScheme.error,
        )
    }
}

@Composable
private fun Message(
    title: String,
    hint: String?,
    action: String?,
    onAction: () -> Unit,
    modifier: Modifier = Modifier,
    pending: Boolean = false,
    icon: (@Composable () -> Unit)? = null,
) {
    Centered(modifier) {
        Column(
            Modifier.widthIn(max = 360.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(Spacing.s),
        ) {
            icon?.invoke()
            Text(title, style = MaterialTheme.typography.titleMedium, textAlign = TextAlign.Center)
            if (hint != null) {
                Text(
                    hint,
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    textAlign = TextAlign.Center,
                )
            }
            if (action != null) {
                PendingButton(
                    action,
                    onAction,
                    Modifier.padding(top = Spacing.s),
                    pending = pending,
                    emphasis = Emphasis.Outlined,
                )
            }
        }
    }
}
