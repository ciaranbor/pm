package dev.pm.app

import android.content.Intent
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.lifecycle.viewmodel.compose.viewModel
import dev.pm.app.push.Notifications
import dev.pm.app.push.Target
import dev.pm.app.ui.App
import dev.pm.app.ui.AppViewModel
import dev.pm.app.ui.PmTheme

class MainActivity : ComponentActivity() {
    /** Where a tapped notification leads, until navigation has gone there. */
    private var target by mutableStateOf<Target?>(null)

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        // A recreated activity restores its back stack; its intent's target was already followed.
        if (savedInstanceState == null) target = Target.from(intent)
        setContent {
            PmTheme {
                val model = viewModel {
                    AppViewModel(container.repository) {
                        Notifications.unsubscribe(applicationContext)
                    }
                }
                App(
                    model,
                    target,
                    targetShown = { target = null },
                    networkChanges = container.networkChanges,
                )
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        target = Target.from(intent)
    }
}
