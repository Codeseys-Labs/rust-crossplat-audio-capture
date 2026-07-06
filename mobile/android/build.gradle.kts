// rsac Android glue — Android library module producing rsac.aar (ADR-0012).
//
// Ownership boundary (ADR-0012 §4.2): this module carries NO capture policy.
// It is consent flow (RsacProjection), service plumbing (RsacCaptureService),
// the Java AudioRecord read loop (CaptureBridge), and name/PID→UID resolution
// helpers (PackageResolver). Target resolution, error classification, and
// stream semantics live in Rust (src/audio/android/).
//
// Source-complete per seed rsac-c4b8; first CI build lands with rsac-1a6e.
// All version pins are "expected; CI trues up".

import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    // AGP 8.7.3 — expected; CI trues up (any AGP 8.x with compileSdk 35 works).
    id("com.android.library") version "8.7.3"
    // Kotlin 2.1.0 — expected; CI trues up.
    id("org.jetbrains.kotlin.android") version "2.1.0"
}

android {
    namespace = "ai.codeseys.rsac"
    // compileSdk 35 — expected; CI trues up. Must be >= 34 so
    // ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROJECTION enforcement paths
    // and the FOREGROUND_SERVICE_MEDIA_PROJECTION permission resolve.
    compileSdk = 35

    defaultConfig {
        // API 29 (Android 10): floor for AudioPlaybackCaptureConfiguration.
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

    // The Rust cdylib (librsac.so) is built by cargo-ndk from the
    // mobile/android-native shim crate (rsac-0aa9) and dropped into
    // src/main/jniLibs/<abi>/ before assembleRelease — the CI mobile-android
    // job does exactly that and asserts the .so lands inside the AAR.
    // jniLibs is picked up by AGP's default sourceSet; nothing to configure.
    // JNI exports arrive with rsac-77f1. See README.md § Native library.

    publishing {
        // Maven/GitHub Packages distribution is a follow-up seed; singleVariant
        // keeps in-tree consumption (`implementation(project(...))` /
        // local AAR) working from day one.
        singleVariant("release") {
            withSourcesJar()
        }
    }
}

kotlin {
    // CI-VERIFY: `kotlin { compilerOptions { } }` is the KGP 2.x DSL; if the
    // resolved KGP rejects it, fall back to
    // `android { kotlinOptions { jvmTarget = "17" } }`.
    compilerOptions {
        jvmTarget.set(JvmTarget.JVM_17)
    }
}

dependencies {
    // Versions expected; CI trues up.
    implementation("androidx.core:core-ktx:1.15.0")       // NotificationCompat, ContextCompat
    implementation("androidx.activity:activity-ktx:1.9.3") // ComponentActivity + ActivityResult API
    implementation("androidx.annotation:annotation:1.9.1")
}
