// rsac-e6d3 (playback tier) — TEST-ONLY host Activity for the MediaProjection
// consent flow. Lives in src/androidTest/ so it never enters the production
// rsac.aar.
package ai.codeseys.rsac

import androidx.activity.ComponentActivity

/**
 * A bare [ComponentActivity] whose only job is to be a started, foreground
 * Activity from which [RsacProjection.request] can launch the system consent
 * dialog and start the `mediaProjection` foreground service
 * (`startActivityForResult` + FGS-while-in-foreground both need a foreground
 * Activity). Declared in the androidTest manifest and driven via
 * `ActivityScenario` by [RsacPlaybackInstrumentedTest].
 */
class RsacTestActivity : ComponentActivity()
