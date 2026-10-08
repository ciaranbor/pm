package dev.pm.app.ui

import org.junit.Assert.assertEquals
import org.junit.Test

class MarkdownPartsTest {
    @Test
    fun a_doc_is_cut_before_headings_but_never_inside_fenced_code() {
        val doc =
            """
            Intro.

            # One
            Body.
            ```sh
            # a comment, not a heading
            ```

            ## Two
            Last.
            """
                .trimIndent()
        assertEquals(
            listOf(
                "Intro.",
                "# One\nBody.\n```sh\n# a comment, not a heading\n```",
                "## Two\nLast.",
            ),
            markdownParts(doc),
        )
    }

    @Test
    fun a_long_stretch_without_headings_is_cut_between_paragraphs_not_inside_a_list_item() {
        val doc = "- item\n\n    continued\n\nsecond\n\nthird"
        assertEquals(
            listOf("- item\n\n    continued", "second", "third"),
            markdownParts(doc, longest = 1),
        )
    }
}
