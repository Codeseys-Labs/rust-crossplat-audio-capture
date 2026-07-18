// tauri-plugin-rsac Android library (ADR-0014). Compile-proof, source-shipped:
// the mobile RUNTIME lane (rsac-e6d3/rsac-97c8) verifies on-device, not this
// lane. Version pins are "expected; CI trues up" — matching the mobile/android
// AAR convention.
//
// This module bridges to the first-party rsac AAR (ai.codeseys.rsac.*,
// mobile/android) and the Tauri Android plugin base classes (app.tauri.*).
// Both are resolved by the Tauri Android build harness / the consuming app's
// Gradle graph; when built standalone in CI they come from the pinned deps
// below and the `:tauri-android` project the Tauri CLI injects.

plugins {
    id("com.android.library")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "ai.codeseys.rsac.tauri"
    compileSdk = 35

    defaultConfig {
        // API 29 (Android 10): floor for AudioPlaybackCaptureConfiguration,
        // matching the rsac AAR (mobile/android/build.gradle.kts).
        minSdk = 29
        consumerProguardFiles("consumer-rules.pro")
    }

    buildTypes {
        release {
            isMinifyEnabled = false
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    kotlinOptions {
        jvmTarget = "17"
    }
}

dependencies {
    implementation("androidx.core:core-ktx:1.15.0")
    implementation("androidx.activity:activity-ktx:1.9.3") // ComponentActivity
    implementation("androidx.annotation:annotation:1.9.1")

    // The Tauri Android plugin base classes (@TauriPlugin, @Command, Invoke,
    // Plugin, JSObject). The Tauri CLI injects `:tauri-android` at build time
    // (see settings.gradle); the consuming app provides it otherwise.
    implementation(project(":tauri-android"))

    // The first-party rsac AAR (RsacProjection consent flow, ADR-0012). In a
    // consuming Tauri app this is the rsac.aar dependency; kept as a
    // compileOnly-style expectation here — the runtime lane wires the concrete
    // coordinate (mobile/android AAR publication is a follow-up seed).
    // implementation("ai.codeseys:rsac:<version>")
}
