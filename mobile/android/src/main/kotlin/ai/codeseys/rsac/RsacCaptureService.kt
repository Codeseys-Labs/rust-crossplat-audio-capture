package ai.codeseys.rsac

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.IBinder
import androidx.core.app.NotificationCompat
import androidx.core.content.ContextCompat
import java.util.concurrent.CopyOnWriteArrayList

/**
 * Foreground service host for MediaProjection-based capture — the
 * `android:foregroundServiceType="mediaProjection"` service required by the
 * OS (docs/MOBILE_BACKEND_DESIGN.md § manifest & service requirements).
 *
 * **Skeleton, no capture policy** (ADR-0012 §4.2): this service does not
 * decide *what* to capture — it provides the foreground context the OS
 * demands, the notification plumbing, and a lifecycle anchor for
 * [CaptureBridge] threads. Target resolution and stream semantics live in
 * Rust (src/audio/android/, seed rsac-77f1).
 *
 * ### Lifecycle contract
 *
 * - [start] must run AFTER MediaProjection consent is granted but before
 *   capture begins — on API 34+ starting a mediaProjection-typed FGS before
 *   consent throws SecurityException (rsac-cabf). [RsacProjection.request]
 *   calls [start] itself on the consent-success path; hosts must not call it
 *   earlier.
 * - [CaptureBridge]s register themselves here ([registerBridge]) while
 *   running; when the service is destroyed — [stop], task removal, or the
 *   system reclaiming it — every registered bridge is stopped so no capture
 *   thread outlives its foreground context. The Rust side observes that as
 *   the producer going quiet and drives its own terminal semantics
 *   (ADR-0010); this service never signals the bridge state machine itself.
 *
 * Notification title/text can be overridden per-start via Intent extras;
 * hosts needing full notification control can post their own and keep this
 * service purely as the FGS anchor.
 */
class RsacCaptureService : Service() {

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        ensureNotificationChannel()
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_STOP -> {
                stopSelf()
            }
            else -> {
                // ACTION_START (or a bare/restart Intent): promote to
                // foreground with the mediaProjection type immediately —
                // required within seconds of startForegroundService().
                val notification = buildNotification(
                    intent?.getStringExtra(EXTRA_NOTIFICATION_TITLE)
                        ?: DEFAULT_NOTIFICATION_TITLE,
                    intent?.getStringExtra(EXTRA_NOTIFICATION_TEXT)
                        ?: DEFAULT_NOTIFICATION_TEXT,
                )
                try {
                    startForeground(
                        NOTIFICATION_ID,
                        notification,
                        ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROJECTION,
                    )
                } catch (e: Exception) {
                    // e.g. ForegroundServiceStartNotAllowedException, or the
                    // pre-consent SecurityException if a host started this
                    // service itself before RsacProjection.request(). Fail the
                    // pending acquisition instead of crashing (rsac-cabf).
                    RsacProjection.onForegroundServiceFailed(
                        "the mediaProjection foreground service failed to start: ${e.message}"
                    )
                    stopSelf()
                    return START_NOT_STICKY
                }
                // The FGS is now confirmed-foreground (startForeground is a
                // synchronous AMS binder call), so this is the earliest legal
                // moment to acquire the MediaProjection (rsac-cabf). No-op
                // unless RsacProjection.request stashed a pending consent.
                RsacProjection.onForegroundServiceReady(this)
            }
        }
        // Capture must not restart without a fresh consent token, so never
        // let the system resurrect this service with a null Intent.
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        // Stop every capture thread anchored to this foreground context.
        // Idempotent per bridge; see CaptureBridge.stop().
        for (bridge in bridges) {
            bridge.stop()
        }
        bridges.clear()
        super.onDestroy()
    }

    private fun ensureNotificationChannel() {
        val manager = getSystemService(NotificationManager::class.java)
        manager.createNotificationChannel(
            NotificationChannel(
                CHANNEL_ID,
                "Audio capture",
                NotificationManager.IMPORTANCE_LOW,
            ).apply {
                description = "Shown while rsac audio capture is running"
            }
        )
    }

    private fun buildNotification(title: String, text: String): Notification =
        NotificationCompat.Builder(this, CHANNEL_ID)
            .setSmallIcon(android.R.drawable.ic_btn_speak_now)
            .setContentTitle(title)
            .setContentText(text)
            .setOngoing(true)
            .setCategory(NotificationCompat.CATEGORY_SERVICE)
            .setForegroundServiceBehavior(NotificationCompat.FOREGROUND_SERVICE_IMMEDIATE)
            .build()

    companion object {
        private const val CHANNEL_ID = "ai.codeseys.rsac.capture"
        private const val NOTIFICATION_ID = 0x52534143 // "RSAC"

        private const val ACTION_START = "ai.codeseys.rsac.action.START"
        private const val ACTION_STOP = "ai.codeseys.rsac.action.STOP"

        /** Intent extra: notification title override (String). */
        const val EXTRA_NOTIFICATION_TITLE = "ai.codeseys.rsac.extra.NOTIFICATION_TITLE"

        /** Intent extra: notification body override (String). */
        const val EXTRA_NOTIFICATION_TEXT = "ai.codeseys.rsac.extra.NOTIFICATION_TEXT"

        private const val DEFAULT_NOTIFICATION_TITLE = "Audio capture active"
        private const val DEFAULT_NOTIFICATION_TEXT =
            "Capturing device audio via screen-capture consent"

        /** Bridges anchored to the service lifecycle (stopped in onDestroy). */
        private val bridges = CopyOnWriteArrayList<CaptureBridge>()

        /**
         * Starts the service in the foreground (mediaProjection type). Call
         * AFTER MediaProjection consent is granted and before capture begins.
         * On API 34+ starting this typed FGS before consent throws
         * SecurityException, so [RsacProjection.request] invokes this on the
         * consent-success path — hosts should not call it earlier (rsac-cabf).
         */
        @JvmStatic
        @JvmOverloads
        fun start(
            context: Context,
            notificationTitle: String? = null,
            notificationText: String? = null,
        ) {
            val intent = Intent(context, RsacCaptureService::class.java)
                .setAction(ACTION_START)
            notificationTitle?.let { intent.putExtra(EXTRA_NOTIFICATION_TITLE, it) }
            notificationText?.let { intent.putExtra(EXTRA_NOTIFICATION_TEXT, it) }
            ContextCompat.startForegroundService(context, intent)
        }

        /** Stops the service (and thereby every registered [CaptureBridge]). */
        @JvmStatic
        fun stop(context: Context) {
            context.startService(
                Intent(context, RsacCaptureService::class.java).setAction(ACTION_STOP)
            )
        }

        /**
         * Anchors [bridge] to the service lifecycle: it will be stopped when
         * the service is destroyed. Called by the capture orchestration
         * (Rust side via JNI, or a native-Kotlin host).
         */
        @JvmStatic
        fun registerBridge(bridge: CaptureBridge) {
            bridges.addIfAbsent(bridge)
        }

        /** Detaches [bridge] (normal stop path, before/after [CaptureBridge.stop]). */
        @JvmStatic
        fun unregisterBridge(bridge: CaptureBridge) {
            bridges.remove(bridge)
        }
    }
}
