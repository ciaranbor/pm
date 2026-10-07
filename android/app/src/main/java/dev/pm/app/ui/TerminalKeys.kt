package dev.pm.app.ui

import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.sizeIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.LocalContentColor
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.unit.dp

/** A key on the sheet: tmux's name for it, what its button shows, and what it is called. */
internal data class Key(val name: String, val label: String, val description: String = label)

internal val Moves =
    listOf(
        Key("Escape", "Esc", "Escape"),
        Key("Tab", "Tab"),
        Key("BTab", "⇧Tab", "Shift Tab"),
        Key("Up", "↑", "Up"),
        Key("Down", "↓", "Down"),
        Key("Left", "←", "Left"),
        Key("Right", "→", "Right"),
        Key("BSpace", "⌫", "Backspace"),
        Key("Space", "Space"),
    )

internal val Digits = ("123456789".toList() + '0').map { Key("$it", "$it") }

internal val Enter = Key("Enter", "Enter ⏎", "Enter")

/** The keys Control may be held with, besides the letters typed. */
private val Controllable = setOf("Up", "Down", "Left", "Right")

/** Control on the key row: off, held for the next key, or held until tapped again. */
enum class Ctrl {
    Off,
    Once,
    Locked;

    /** What a tap makes it: on for one key if off, else off. */
    fun tapped() = if (this == Off) Once else Off

    /**
     * What pressing any key leaves it: held for one key is spent even on a key Control can't
     * modify.
     */
    fun used() = if (this == Once) Off else this

    /** tmux's name for `key` with Control, if held and the key takes it; else `key`. */
    fun on(key: String): String = if (this != Off && key in Controllable) "C-$key" else key
}

/** tmux's name for `letter` pressed with Control. */
fun withCtrl(letter: Char): String = "C-${letter.lowercaseChar()}"

/**
 * One row of keys: the "123" toggle, then Control and the moves (or the digits) scrolling sideways,
 * then Enter. A key shows progress while its press is on its way.
 */
@Composable
internal fun KeyRow(
    digits: Boolean,
    toggleDigits: () -> Unit,
    ctrl: Ctrl,
    setCtrl: (Ctrl) -> Unit,
    pressing: List<String>,
    press: (Key) -> Unit,
    modifier: Modifier = Modifier,
) {
    fun pending(key: Key) = pressing.any { it == key.name || it == "C-${key.name}" }
    Row(
        modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.spacedBy(Spacing.xs),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        KeyButton(
            "123",
            "Digits",
            onClick = toggleDigits,
            state = if (digits) "On" else "Off",
            style = if (digits) KeyStyle.Held else KeyStyle.Plain,
        )
        Row(
            Modifier.weight(1f)
                .clip(MaterialTheme.shapes.small)
                .horizontalScroll(rememberScrollState()),
            horizontalArrangement = Arrangement.spacedBy(Spacing.xs),
        ) {
            if (digits) {
                Digits.forEach { key ->
                    KeyButton(key.label, key.description, { press(key) }, pending = pending(key))
                }
            } else {
                Moves.take(3).forEach { key ->
                    KeyButton(key.label, key.description, { press(key) }, pending = pending(key))
                }
                KeyButton(
                    "Ctrl",
                    "Control",
                    onClick = { setCtrl(ctrl.tapped()) },
                    onLongClick = { setCtrl(Ctrl.Locked) },
                    state =
                        when (ctrl) {
                            Ctrl.Off -> "Off"
                            Ctrl.Once -> "On for one key"
                            Ctrl.Locked -> "Locked"
                        },
                    style =
                        when (ctrl) {
                            Ctrl.Off -> KeyStyle.Plain
                            Ctrl.Once -> KeyStyle.Held
                            Ctrl.Locked -> KeyStyle.Locked
                        },
                )
                Moves.drop(3).forEach { key ->
                    KeyButton(key.label, key.description, { press(key) }, pending = pending(key))
                }
            }
        }
        KeyButton(
            Enter.label,
            Enter.description,
            { press(Enter) },
            pending = pending(Enter),
            style = KeyStyle.Primary,
        )
    }
}

private enum class KeyStyle {
    Plain,
    Held,
    Locked,
    Primary,
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun KeyButton(
    label: String,
    description: String,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    onLongClick: (() -> Unit)? = null,
    pending: Boolean = false,
    state: String? = null,
    style: KeyStyle = KeyStyle.Plain,
) {
    val colors = MaterialTheme.colorScheme
    val (background, content) =
        when (style) {
            KeyStyle.Plain -> colors.secondaryContainer to colors.onSecondaryContainer
            KeyStyle.Held -> colors.tertiaryContainer to colors.onTertiaryContainer
            KeyStyle.Locked -> colors.tertiary to colors.onTertiary
            KeyStyle.Primary -> colors.primary to colors.onPrimary
        }
    val shape = MaterialTheme.shapes.small
    Box(
        modifier
            .sizeIn(minWidth = 44.dp, minHeight = 40.dp)
            .clip(shape)
            .background(background)
            .then(
                if (style == KeyStyle.Locked)
                    Modifier.border(2.dp, colors.onTertiaryContainer, shape)
                else Modifier
            )
            .combinedClickable(onClick = onClick, onLongClick = onLongClick, role = Role.Button)
            .semantics {
                contentDescription = description
                (state ?: "In progress".takeIf { pending })?.let { stateDescription = it }
            },
        contentAlignment = Alignment.Center,
    ) {
        CompositionLocalProvider(LocalContentColor provides content) {
            Text(
                label,
                style = MaterialTheme.typography.labelLarge,
                color = content,
                modifier =
                    Modifier.padding(horizontal = Spacing.m, vertical = Spacing.s)
                        .alpha(if (pending) 0f else 1f),
            )
            if (pending) {
                CircularProgressIndicator(
                    Modifier.size(16.dp),
                    color = content,
                    strokeWidth = 2.dp,
                    trackColor = Color.Transparent,
                )
            }
        }
    }
}
