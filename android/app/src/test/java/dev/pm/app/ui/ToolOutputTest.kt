package dev.pm.app.ui

import org.junit.Assert.assertEquals
import org.junit.Test

class ToolOutputTest {
    @Test
    fun tabs_expand_to_stops_of_eight() {
        assertEquals(listOf("a       b", "abcdefgh        c"), outputLines("a\tb\nabcdefgh\tc"))
    }

    @Test
    fun a_line_wider_than_a_row_goes_on_over_the_rows_after_it() {
        assertEquals(listOf("abcd", "efgh", "ij", "k"), outputLines("abcdefghij\nk", width = 4))
    }
}
