package dev.pm.app.model

import kotlinx.serialization.Serializable

/** A project's notes as the server last had them; `version` is what a save names. */
@Serializable data class Notes(val text: String, val version: String)

/** An edit of the notes not yet saved: its text, and the version it started from. */
@Serializable data class NotesDraft(val base: Notes, val text: String)

/** The longest notes the server takes from a phone, in UTF-8 bytes; `pm notes` has no limit. */
const val MAX_NOTES_BYTES = 256 * 1024

/** Whether `text` is longer than the server takes from a phone. */
fun overNotesLimit(text: String): Boolean =
    text.length > MAX_NOTES_BYTES || text.encodeToByteArray().size > MAX_NOTES_BYTES
