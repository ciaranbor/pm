package dev.pm.app.ui

import android.Manifest
import android.content.pm.PackageManager
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.camera.core.CameraSelector
import androidx.camera.core.ImageAnalysis
import androidx.camera.core.ImageProxy
import androidx.camera.core.Preview
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.camera.view.PreviewView
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.core.content.ContextCompat
import androidx.lifecycle.compose.LocalLifecycleOwner
import com.google.mlkit.vision.barcode.BarcodeScannerOptions
import com.google.mlkit.vision.barcode.BarcodeScanning
import com.google.mlkit.vision.barcode.common.Barcode
import com.google.mlkit.vision.common.InputImage
import dev.pm.app.model.Pairing
import java.util.concurrent.Executors

@Composable
fun PairScreen(paired: (Pairing) -> Unit) {
    val context = LocalContext.current
    var granted by remember {
        mutableStateOf(ContextCompat.checkSelfPermission(context, Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED)
    }
    val ask = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { granted = it }
    var pasted by remember { mutableStateOf("") }
    var problem by remember { mutableStateOf<String?>(null) }

    Column(
        Modifier.verticalScroll(rememberScrollState()).padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Text("Pair with pm serve", style = MaterialTheme.typography.headlineSmall)
        Text("On the Mac, run `pm serve pair --name <this phone>` and scan the QR code it prints.")
        if (granted) {
            Scanner(onPairing = paired, onOther = { problem = "That QR code is not a pm pairing." })
        } else {
            Button(onClick = { ask.launch(Manifest.permission.CAMERA) }) { Text("Scan the QR code") }
        }
        problem?.let { Text(it, color = MaterialTheme.colorScheme.error) }
        Text("Or paste the pairing JSON, or url, device and token as `pair` prints them:", style = MaterialTheme.typography.bodySmall)
        OutlinedTextField(
            value = pasted,
            onValueChange = { pasted = it },
            modifier = Modifier.fillMaxWidth(),
            minLines = 3,
        )
        TextButton(onClick = {
            val pairing = Pairing.parse(pasted)
            if (pairing == null) problem = "That is not a pairing." else paired(pairing)
        }) { Text("Pair") }
    }
}

/**
 * A camera preview reading QR codes. pm draws its code for a dark terminal,
 * light on dark, which a scanner may not read the right way round; every
 * other frame is inverted before it is scanned.
 */
@Composable
private fun Scanner(onPairing: (Pairing) -> Unit, onOther: () -> Unit) {
    val context = LocalContext.current
    val owner = LocalLifecycleOwner.current
    val executor = remember { Executors.newSingleThreadExecutor() }
    val scanner = remember {
        BarcodeScanning.getClient(BarcodeScannerOptions.Builder().setBarcodeFormats(Barcode.FORMAT_QR_CODE).build())
    }
    var done by remember { mutableStateOf(false) }
    val preview = remember { PreviewView(context) }

    DisposableEffect(owner) {
        val future = ProcessCameraProvider.getInstance(context)
        var frame = 0L
        future.addListener({
            val provider = future.get()
            val shown = Preview.Builder().build().also { it.surfaceProvider = preview.surfaceProvider }
            val analysis = ImageAnalysis.Builder()
                .setBackpressureStrategy(ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST)
                .build()
            analysis.setAnalyzer(executor) { proxy ->
                val image = grey(proxy, invert = frame++ % 2 == 1L)
                scanner.process(image)
                    .addOnSuccessListener { codes ->
                        val text = codes.firstNotNullOfOrNull { it.rawValue } ?: return@addOnSuccessListener
                        if (done) return@addOnSuccessListener
                        val pairing = Pairing.parse(text)
                        if (pairing == null) {
                            onOther()
                        } else {
                            done = true
                            onPairing(pairing)
                        }
                    }
                    .addOnCompleteListener { proxy.close() }
            }
            provider.unbindAll()
            provider.bindToLifecycle(owner, CameraSelector.DEFAULT_BACK_CAMERA, shown, analysis)
        }, ContextCompat.getMainExecutor(context))
        onDispose {
            runCatching { future.get().unbindAll() }
            scanner.close()
            executor.shutdown()
        }
    }
    AndroidView(factory = { preview }, modifier = Modifier.fillMaxWidth().aspectRatio(1f))
}

/** The frame's luminance as an NV21 image, inverted on request; colour plays no part in a QR code. */
private fun grey(proxy: ImageProxy, invert: Boolean): InputImage {
    val plane = proxy.planes[0]
    val width = proxy.width
    val height = proxy.height
    val nv21 = ByteArray(width * height * 3 / 2) { 128.toByte() }
    val buffer = plane.buffer
    for (row in 0 until height) {
        buffer.position(row * plane.rowStride)
        buffer.get(nv21, row * width, width)
    }
    if (invert) for (i in 0 until width * height) nv21[i] = (255 - (nv21[i].toInt() and 0xFF)).toByte()
    return InputImage.fromByteArray(nv21, width, height, proxy.imageInfo.rotationDegrees, InputImage.IMAGE_FORMAT_NV21)
}
