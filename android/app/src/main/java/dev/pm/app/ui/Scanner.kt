package dev.pm.app.ui

import android.content.Context
import androidx.camera.core.CameraSelector
import androidx.camera.core.ImageAnalysis
import androidx.camera.core.ImageProxy
import androidx.camera.core.Preview
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.camera.view.PreviewView
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.viewinterop.AndroidView
import androidx.core.content.ContextCompat
import androidx.lifecycle.LifecycleOwner
import androidx.lifecycle.Observer
import androidx.lifecycle.compose.LocalLifecycleOwner
import dev.pm.app.model.Pairing
import java.util.concurrent.Executor
import java.util.concurrent.Executors
import kotlin.time.Duration.Companion.seconds
import kotlinx.coroutines.delay

/**
 * A camera preview reading QR codes. A phone whose camera can't be opened, though it has the
 * feature and the permission (an emulator with none), calls `onUnavailable`: binding fails, or
 * CameraX finishes initialising without the camera and the preview never streams.
 */
@Composable
internal fun Scanner(onPairing: (Pairing) -> Unit, onOther: () -> Unit, onUnavailable: () -> Unit) {
    val context = LocalContext.current
    val owner = LocalLifecycleOwner.current
    val executor = remember { Executors.newSingleThreadExecutor() }
    var done by remember { mutableStateOf(false) }
    var streaming by remember { mutableStateOf(false) }
    // A TextureView, which Compose clips; a SurfaceView draws over what lies outside its box.
    val preview = remember {
        PreviewView(context).apply {
            implementationMode = PreviewView.ImplementationMode.COMPATIBLE
        }
    }
    val unavailable by rememberUpdatedState(onUnavailable)

    DisposableEffect(owner) {
        var disposed = false
        val future = ProcessCameraProvider.getInstance(context)
        future.addListener(
            {
                try {
                    val provider = future.get()
                    if (disposed) {
                        provider.unbindAll()
                        return@addListener
                    }
                    if (!provider.hasCamera(CameraSelector.DEFAULT_BACK_CAMERA)) {
                        unavailable()
                        return@addListener
                    }
                    bind(provider, owner, preview, executor, context) { pairing ->
                        if (done || disposed) return@bind
                        if (pairing == null) {
                            onOther()
                        } else {
                            done = true
                            onPairing(pairing)
                        }
                    }
                } catch (_: Exception) {
                    if (!disposed) unavailable()
                }
            },
            ContextCompat.getMainExecutor(context),
        )
        val state = preview.previewStreamState
        val observer =
            Observer<PreviewView.StreamState> {
                if (it == PreviewView.StreamState.STREAMING) streaming = true
            }
        state.observe(owner, observer)
        onDispose {
            disposed = true
            state.removeObserver(observer)
            // Unready, the provider is unbound by the listener: get() would block until it is.
            if (future.isDone) runCatching { future.get().unbindAll() }
            executor.shutdown()
        }
    }
    LaunchedEffect(Unit) {
        delay(STREAM_TIMEOUT)
        if (!streaming) unavailable()
    }
    AndroidView(
        factory = { preview },
        modifier = Modifier.fillMaxWidth().aspectRatio(1f).clip(MaterialTheme.shapes.medium),
    )
}

/**
 * How long a camera may take to start streaming. CameraX alone retries a missing camera for about 5
 * s before giving up.
 */
private val STREAM_TIMEOUT = 10.seconds

/** Show the back camera in `preview`, passing each QR code read, as a pairing if it is one. */
private fun bind(
    provider: ProcessCameraProvider,
    owner: LifecycleOwner,
    preview: PreviewView,
    executor: Executor,
    context: Context,
    read: (Pairing?) -> Unit,
) {
    val shown = Preview.Builder().build().also { it.surfaceProvider = preview.surfaceProvider }
    val analysis =
        ImageAnalysis.Builder()
            .setBackpressureStrategy(ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST)
            .build()
    analysis.setAnalyzer(executor) { proxy ->
        val text =
            proxy.use { QrDecoder.decode(luminance(it), it.width, it.height) } ?: return@setAnalyzer
        val pairing = Pairing.parse(text)
        ContextCompat.getMainExecutor(context).execute { read(pairing) }
    }
    provider.unbindAll()
    provider.bindToLifecycle(owner, CameraSelector.DEFAULT_BACK_CAMERA, shown, analysis)
}

/** The frame's luminance plane, row by row without padding; colour plays no part in a QR code. */
private fun luminance(proxy: ImageProxy): ByteArray {
    val plane = proxy.planes[0]
    val width = proxy.width
    val pixels = ByteArray(width * proxy.height)
    val buffer = plane.buffer
    for (row in 0 until proxy.height) {
        buffer.position(row * plane.rowStride)
        buffer.get(pixels, row * width, width)
    }
    return pixels
}
