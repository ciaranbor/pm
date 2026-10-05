package dev.pm.app.ui

import com.google.zxing.BarcodeFormat
import com.google.zxing.BinaryBitmap
import com.google.zxing.DecodeHintType
import com.google.zxing.MultiFormatReader
import com.google.zxing.PlanarYUVLuminanceSource
import com.google.zxing.ReaderException
import com.google.zxing.common.HybridBinarizer

/**
 * Reads a QR code from a greyscale frame. pm draws its code for a dark terminal, light on dark, so
 * the inverted frame is tried too.
 */
object QrDecoder {
    private val hints =
        mapOf(
            DecodeHintType.POSSIBLE_FORMATS to listOf(BarcodeFormat.QR_CODE),
            DecodeHintType.ALSO_INVERTED to true,
            DecodeHintType.TRY_HARDER to true,
        )

    /** The text of the QR code in `luminance` (one byte per pixel, row by row), if one is read. */
    fun decode(luminance: ByteArray, width: Int, height: Int): String? {
        val source = PlanarYUVLuminanceSource(luminance, width, height, 0, 0, width, height, false)
        return try {
            // MultiFormatReader, not QRCodeReader: it is what honours ALSO_INVERTED.
            MultiFormatReader().decode(BinaryBitmap(HybridBinarizer(source)), hints).text
        } catch (_: ReaderException) {
            null
        }
    }
}
