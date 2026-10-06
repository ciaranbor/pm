package dev.pm.app.ui

import dev.pm.app.api.PmError

/** What to update, the app or pm on the server, so that `doThis` ("type here") works. */
fun PmError.Unsupported.advice(doThis: String): String =
    if (retired) "This app is older than pm on the server; update the app to $doThis."
    else "pm on the server is older than this app; update it to $doThis."
