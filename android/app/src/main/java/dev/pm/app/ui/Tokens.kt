package dev.pm.app.ui

import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Shapes
import androidx.compose.ui.unit.dp

/** The app's spacing steps; layouts use these rather than their own numbers. */
object Spacing {
    val xxs = 2.dp
    val xs = 4.dp
    val s = 8.dp
    val m = 12.dp
    val l = 16.dp
    val xl = 24.dp

    /** The side margin of a screen's content. */
    val gutter = l
}

/** How dense list rows are: their padding, and where a divider between them starts. */
object Rows {
    val padding = PaddingValues(horizontal = Spacing.gutter, vertical = Spacing.s)

    /** Dividers start at the text, not the screen's edge. */
    val dividerInset = Spacing.gutter
}

/** Corner radii by size of the thing rounded. */
val PmShapes =
    Shapes(
        extraSmall = RoundedCornerShape(4.dp),
        small = RoundedCornerShape(8.dp),
        medium = RoundedCornerShape(12.dp),
        large = RoundedCornerShape(16.dp),
        extraLarge = RoundedCornerShape(28.dp),
    )
