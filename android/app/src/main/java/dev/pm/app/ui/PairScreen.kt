package dev.pm.app.ui

import android.Manifest
import android.content.pm.PackageManager
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.wrapContentHeight
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import dev.pm.app.R
import dev.pm.app.model.Pairing

/** How the pairing reaches the phone. */
private enum class Method {
    Scan,
    Paste,
}

/**
 * Pairing in two steps: run `pm serve pair`, then scan its QR code or paste what it prints. A phone
 * without a usable camera pastes.
 */
@Composable
fun PairScreen(
    paired: (Pairing) -> Unit,
    modifier: Modifier = Modifier,
    hasCamera: Boolean =
        LocalContext.current.packageManager.hasSystemFeature(PackageManager.FEATURE_CAMERA_ANY),
) {
    val context = LocalContext.current
    var method by rememberSaveable { mutableStateOf(if (hasCamera) null else Method.Paste) }
    var cameraProblem by rememberSaveable { mutableStateOf<String?>(null) }
    val ask =
        rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
            if (granted) method = Method.Scan
            else {
                cameraProblem = "Without the camera, paste the pairing instead."
                method = Method.Paste
            }
        }
    val scan = {
        cameraProblem = null
        val granted =
            ContextCompat.checkSelfPermission(context, Manifest.permission.CAMERA) ==
                PackageManager.PERMISSION_GRANTED
        if (granted) method = Method.Scan else ask.launch(Manifest.permission.CAMERA)
    }

    Column(
        modifier.verticalScroll(rememberScrollState()).padding(Spacing.gutter),
        verticalArrangement = Arrangement.spacedBy(Spacing.xl),
    ) {
        Step(1, "On the server, run") {
            Selectable {
                Text(
                    "pm serve pair --name <this phone>",
                    style =
                        MaterialTheme.typography.bodyMedium.copy(fontFamily = FontFamily.Monospace),
                    modifier =
                        Modifier.fillMaxWidth()
                            .background(
                                MaterialTheme.colorScheme.surfaceContainerHigh,
                                MaterialTheme.shapes.small,
                            )
                            .padding(Spacing.m),
                )
            }
        }
        Step(
            2,
            if (hasCamera) "Scan the QR code it prints, or paste it" else "Paste what it prints",
        ) {
            if (hasCamera) {
                Row(horizontalArrangement = Arrangement.spacedBy(Spacing.s)) {
                    val icon: @Composable (Int) -> Unit = {
                        Icon(painterResource(it), null, Modifier.size(ButtonDefaults.IconSize))
                    }
                    Button(onClick = scan) {
                        icon(R.drawable.ic_qr_code_scanner)
                        Text("Scan QR", Modifier.padding(start = Spacing.s))
                    }
                    OutlinedButton(onClick = { method = Method.Paste }) {
                        icon(R.drawable.ic_content_paste)
                        Text("Paste", Modifier.padding(start = Spacing.s))
                    }
                }
            }
            cameraProblem?.let {
                Text(
                    it,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    style = MaterialTheme.typography.bodyMedium,
                )
            }
            when (method) {
                Method.Scan ->
                    ScanStep(paired) { reason ->
                        cameraProblem = reason
                        method = Method.Paste
                    }
                Method.Paste -> PasteStep(paired)
                null -> {}
            }
        }
    }
}

/** A numbered step: what to do, and what it takes. */
@Composable
private fun Step(number: Int, title: String, content: @Composable () -> Unit) {
    Row(horizontalArrangement = Arrangement.spacedBy(Spacing.m)) {
        Box(
            contentAlignment = Alignment.Center,
            modifier =
                Modifier.size(28.dp)
                    .background(MaterialTheme.colorScheme.primaryContainer, CircleShape),
        ) {
            Text(
                "$number",
                style = MaterialTheme.typography.labelLarge,
                color = MaterialTheme.colorScheme.onPrimaryContainer,
            )
        }
        Column(
            Modifier.weight(1f),
            verticalArrangement = Arrangement.spacedBy(Spacing.m),
        ) {
            Text(
                title,
                style = MaterialTheme.typography.titleMedium,
                modifier =
                    Modifier.heightIn(min = 28.dp).wrapContentHeight(Alignment.CenterVertically),
            )
            content()
        }
    }
}

@Composable
private fun ScanStep(paired: (Pairing) -> Unit, unavailable: (String) -> Unit) {
    var problem by remember { mutableStateOf<String?>(null) }
    Column(verticalArrangement = Arrangement.spacedBy(Spacing.s)) {
        Scanner(
            onPairing = paired,
            onOther = { problem = "That QR code is not a pm pairing." },
            onUnavailable = {
                unavailable("The camera isn't available, so paste the pairing instead.")
            },
        )
        problem?.let { Text(it, color = MaterialTheme.colorScheme.error) }
    }
}

@Composable
private fun PasteStep(paired: (Pairing) -> Unit) {
    var pasted by rememberSaveable { mutableStateOf("") }
    var invalid by rememberSaveable { mutableStateOf(false) }
    val submit = {
        val pairing = Pairing.parse(pasted)
        if (pairing == null) invalid = true else paired(pairing)
    }
    Column(verticalArrangement = Arrangement.spacedBy(Spacing.s)) {
        OutlinedTextField(
            value = pasted,
            onValueChange = {
                pasted = it
                invalid = false
            },
            label = { Text("Pairing") },
            supportingText = {
                Text(
                    if (invalid) "That is not a pairing: paste all that pm serve pair prints."
                    else "The JSON, or the url, device and token lines"
                )
            },
            isError = invalid,
            modifier = Modifier.fillMaxWidth(),
            minLines = 3,
        )
        Button(
            onClick = submit,
            enabled = pasted.isNotBlank(),
            modifier = Modifier.fillMaxWidth(),
        ) {
            Text("Pair")
        }
    }
}
