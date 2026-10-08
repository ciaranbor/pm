package dev.pm.app.model

import kotlinx.serialization.Serializable

/**
 * A category of a project's information store, as its `categories.toml` lists it; `size` is 0 and
 * `modified` null for a doc not written yet.
 */
@Serializable
data class DocCategory(
    val filename: String,
    val description: String = "",
    val size: Long = 0,
    val modified: String? = null,
) {
    val title: String
        get() = docTitle(filename)
}

/** A doc's filename as its title: `user-feedback.md` as `User feedback`. */
fun docTitle(filename: String): String =
    filename.substringBeforeLast('.').replace('-', ' ').replace('_', ' ').replaceFirstChar {
        it.uppercase()
    }
