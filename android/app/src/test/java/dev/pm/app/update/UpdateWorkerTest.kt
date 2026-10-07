package dev.pm.app.update

import android.app.Notification
import android.app.NotificationManager
import android.content.Context
import androidx.test.core.app.ApplicationProvider
import dev.pm.app.container
import dev.pm.app.push.Notifications
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows.shadowOf

@RunWith(RobolectricTestRunner::class)
class UpdateWorkerTest {
    private val context: Context = ApplicationProvider.getApplicationContext()

    @Test
    fun an_offered_update_posts_its_notification_and_the_daily_check_skips_it() {
        Notifications.createChannels(context)
        UpdateWorker.offer(context, Update("9.9.9", "https://example.com/pm.apk"))

        val posted =
            context.getSystemService(NotificationManager::class.java).activeNotifications.single()
        assertEquals("app-update", posted.notification.channelId)
        assertEquals(
            "pm 9.9.9 is available",
            posted.notification.extras.getString(Notification.EXTRA_TITLE),
        )
        assertEquals("9.9.9", context.container.store.notifiedUpdate)
    }

    @Test
    fun an_update_offered_while_notifications_are_off_is_offered_again_later() {
        Notifications.createChannels(context)
        shadowOf(context.getSystemService(NotificationManager::class.java))
            .setNotificationsEnabled(false)
        UpdateWorker.offer(context, Update("9.9.9", "https://example.com/pm.apk"))

        assertNull(context.container.store.notifiedUpdate)
    }
}
