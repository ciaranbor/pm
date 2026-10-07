package dev.pm.app.ui

/**
 * `text` as rows of at most `width` columns, tabs expanded to stops of eight: a longer line goes on
 * over the rows after it, so no row is wider than a screen can be laid out.
 */
fun outputLines(text: String, width: Int = MAX_OUTPUT_COLUMNS): List<String> =
    text.lines().flatMap { line ->
        val expanded =
            if ('\t' !in line) line
            else
                buildString {
                    line.forEach { c ->
                        if (c == '\t') repeat(TAB - length % TAB) { append(' ') } else append(c)
                    }
                }
        if (expanded.length <= width) listOf(expanded) else expanded.chunked(width)
    }

private const val TAB = 8
private const val MAX_OUTPUT_COLUMNS = 1000
