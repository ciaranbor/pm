package dev.pm.app.ui

import android.content.Intent
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalClipboard
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.painterResource
import dev.pm.app.R
import kotlinx.coroutines.launch

/**
 * What can be done with a message: copy it, read it whole to select some of it (`select`), or share
 * it. Each closes the sheet (`dismiss`).
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun MessageActions(
    reading: Reading,
    select: () -> Unit,
    dismiss: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val clipboard = LocalClipboard.current
    val context = LocalContext.current
    val feedback = LocalFeedback.current
    val scope = rememberCoroutineScope()
    ModalBottomSheet(onDismissRequest = dismiss, modifier = modifier) {
        Column(Modifier.padding(bottom = Spacing.l)) {
            Action(R.drawable.ic_content_copy, "Copy") {
                scope.launch {
                    clipboard.copy(reading.title, reading.text) { feedback.done("Copied") }
                    dismiss()
                }
            }
            Action(R.drawable.ic_select_all, "Select text") {
                dismiss()
                select()
            }
            Action(R.drawable.ic_share, "Share") {
                val send =
                    Intent(Intent.ACTION_SEND)
                        .setType("text/plain")
                        .putExtra(Intent.EXTRA_TEXT, reading.text)
                context.startActivity(Intent.createChooser(send, null))
                dismiss()
            }
        }
    }
}

@Composable
private fun Action(icon: Int, label: String, onClick: () -> Unit) {
    ListItem(
        headlineContent = { Text(label) },
        leadingContent = { Icon(painterResource(icon), null) },
        colors = ListItemDefaults.colors(),
        modifier = Modifier.clickable(onClick = onClick),
    )
}
