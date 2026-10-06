package dev.pm.app.ui

import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.em
import com.mikepenz.markdown.compose.components.MarkdownComponents
import com.mikepenz.markdown.compose.components.markdownComponents
import com.mikepenz.markdown.m3.Markdown
import com.mikepenz.markdown.m3.markdownTypography
import com.mikepenz.markdown.model.MarkdownAnnotator
import com.mikepenz.markdown.model.markdownAnnotator
import com.mikepenz.markdown.model.rememberMarkdownState
import org.intellij.markdown.MarkdownElementTypes
import org.intellij.markdown.ast.getTextInNode

/**
 * Markdown in the app's type: headings at title sizes, since display sizes fill a phone screen with
 * one line, and `text` for the body; `components` draws its parts, which read these styles. Parsed
 * off the main thread; the plain text holds its place until then.
 */
@Composable
fun PmMarkdown(
    markdown: String,
    modifier: Modifier = Modifier,
    text: TextStyle = MaterialTheme.typography.bodyLarge,
    components: MarkdownComponents = markdownComponents(),
) {
    val state = rememberMarkdownState(markdown, retainState = true)
    val type = MaterialTheme.typography
    val code = text.copy(fontFamily = FontFamily.Monospace, fontSize = 0.9.em)
    val codeBackground = MaterialTheme.colorScheme.surfaceContainerHighest
    val annotator =
        remember(code, codeBackground) {
            tightCode(code.toSpanStyle().copy(background = codeBackground))
        }
    Markdown(
        markdownState = state,
        typography =
            markdownTypography(
                h1 = type.titleLarge,
                h2 = type.titleMedium,
                h3 = type.titleSmall,
                h4 = type.titleSmall,
                h5 = type.titleSmall,
                h6 = type.titleSmall,
                text = text,
                paragraph = text,
                ordered = text,
                bullet = text,
                list = text,
                table = text,
                code = type.bodySmall.copy(fontFamily = FontFamily.Monospace),
                inlineCode = code,
                quote = text.copy(color = MaterialTheme.colorScheme.onSurfaceVariant),
            ),
        annotator = annotator,
        components = components,
        modifier = modifier,
        loading = { Text(markdown, it, style = text) },
    )
}

/**
 * Inline code in `style`, without the space the renderer pads each side with: a monospace space is
 * wide, and a sentence with several code spans reads as gappy.
 */
private fun tightCode(style: SpanStyle): MarkdownAnnotator = markdownAnnotator { content, child ->
    if (child.type != MarkdownElementTypes.CODE_SPAN) return@markdownAnnotator false
    val inner = child.children.drop(1).dropLast(1)
    withStyle(style) { inner.forEach { append(it.getTextInNode(content)) } }
    true
}
