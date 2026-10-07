package dev.pm.app.ui

import android.icu.lang.UCharacter
import android.icu.lang.UCharacterEnums.ECharacterCategory
import android.icu.lang.UProperty

/**
 * A pane's rows as the terminal sheet shows them: without the blank rows below what was drawn, and
 * each run of blank rows between parts of the screen as one.
 */
fun screenRows(text: String): List<String> {
    val rows = text.lines().map { it.trimEnd() }.dropLastWhile { it.isEmpty() }
    return rows.filterIndexed { i, row -> row.isNotEmpty() || i == 0 || rows[i - 1].isNotEmpty() }
}

/**
 * How many terminal cells `codePoint` takes: two for East Asian wide and fullwidth characters (CJK,
 * and emoji drawn as emoji), none for a mark joined to the character before it, else one.
 */
fun cellWidth(codePoint: Int): Int {
    if (codePoint == ZWJ || codePoint in 0xFE00..0xFE0F) return 0
    when (UCharacter.getType(codePoint).toByte()) {
        ECharacterCategory.NON_SPACING_MARK,
        ECharacterCategory.ENCLOSING_MARK -> return 0
    }
    return when (UCharacter.getIntPropertyValue(codePoint, UProperty.EAST_ASIAN_WIDTH)) {
        UCharacter.EastAsianWidth.WIDE,
        UCharacter.EastAsianWidth.FULLWIDTH -> 2
        else -> 1
    }
}

private const val ZWJ = 0x200D

/** One character on the grid: its text, at `column`, `width` cells wide. */
data class Cell(val column: Int, val text: String, val width: Int)

/** `row`'s characters on the grid, a mark kept with the character it joins. */
fun cells(row: String): List<Cell> {
    val cells = mutableListOf<Cell>()
    var column = 0
    var i = 0
    while (i < row.length) {
        val codePoint = row.codePointAt(i)
        val char = String(Character.toChars(codePoint))
        i += char.length
        val width = cellWidth(codePoint)
        val last = cells.lastOrNull()
        if (width == 0 && last != null) {
            cells[cells.lastIndex] = last.copy(text = last.text + char)
        } else {
            cells += Cell(column, char, maxOf(width, 1))
            column += maxOf(width, 1)
        }
    }
    return cells
}

/** How many cells wide `row` is. */
fun columns(row: String): Int = cells(row).lastOrNull()?.let { it.column + it.width } ?: 0

/**
 * The links on the screen, in order. A link cut at the end of a row the pane wrapped (a row as wide
 * as the widest, or one the link fills up to a box's border) carries on with the link characters
 * that start the next row.
 */
fun screenLinks(rows: List<String>): List<String> {
    val plain = rows.map { row -> row.map { if (it.code in BOX) ' ' else it }.joinToString("") }
    val widest = rows.maxOfOrNull { columns(it) } ?: 0
    fun cut(row: Int, at: Int) =
        at == rows[row].trimEnd().lastIndex && columns(rows[row]) >= widest - 2 ||
            rows[row].getOrNull(at + 1)?.code in BOX
    val links = mutableListOf<String>()
    var i = 0
    var from = 0
    while (i < plain.size) {
        val found = LINK.findAll(plain[i], from).toList()
        from = 0
        var next = i + 1
        found.forEachIndexed { n, match ->
            var link = match.value
            var row = i
            var at = match.range.last
            while (n == found.lastIndex && row + 1 < plain.size && cut(row, at)) {
                val line = plain[row + 1]
                val start = line.indexOfFirst { it != ' ' }
                if (start < 0 || LINK.find(line, start)?.range?.first == start) break
                val end =
                    (start until line.length).firstOrNull { !isLinkChar(line[it]) } ?: line.length
                if (end == start) break
                link += line.substring(start, end)
                row++
                at = end - 1
            }
            if (row > i) {
                val rest = at < plain[row].trimEnd().lastIndex
                next = if (rest) row else row + 1
                if (rest) from = at + 1
            }
            links += link.trimEnd(*TRAILING)
        }
        i = next
    }
    return links
}

private val BOX = 0x2500..0x257F

private val LINK = Regex("""https?://\S+""")

private val TRAILING = charArrayOf('.', ',', ';', ':', '!', '?', ')', ']', '"', '\'', '>')

private fun isLinkChar(c: Char) = c.code in 0x21..0x7E && c != '<' && c != '>' && c != '"'
