package dev.pm.app.ui

import com.google.zxing.BarcodeFormat
import com.google.zxing.EncodeHintType
import com.google.zxing.qrcode.QRCodeWriter
import org.junit.Assert.assertEquals
import org.junit.Test

class QrDecoderTest {
    private val pairing =
        """{"url":"https://mac.tail1234.ts.net","device":"pixel","token":"0123456789abcdef0123456789abcdef"}"""

    /** `text` as a camera frame's luminance, `pixels` per module, dark modules drawn `dark`. */
    private fun frame(
        text: String,
        dark: Int,
        light: Int,
        pixels: Int = 4,
    ): Triple<ByteArray, Int, Int> {
        val matrix =
            QRCodeWriter()
                .encode(text, BarcodeFormat.QR_CODE, 0, 0, mapOf(EncodeHintType.MARGIN to 4))
        val width = matrix.width * pixels
        val height = matrix.height * pixels
        val luminance =
            ByteArray(width * height) { i ->
                val on = matrix.get(i % width / pixels, i / width / pixels)
                (if (on) dark else light).toByte()
            }
        return Triple(luminance, width, height)
    }

    @Test
    fun a_code_drawn_light_on_dark_as_pm_draws_it_is_read() {
        val (luminance, width, height) = frame(pairing, dark = 230, light = 20)
        assertEquals(pairing, QrDecoder.decode(luminance, width, height))
    }

    @Test
    fun a_code_drawn_dark_on_light_is_read_too() {
        val (luminance, width, height) = frame(pairing, dark = 20, light = 230)
        assertEquals(pairing, QrDecoder.decode(luminance, width, height))
    }
}
