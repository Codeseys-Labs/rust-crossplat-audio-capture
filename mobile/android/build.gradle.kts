// rsac Android glue — Android library module producing rsac.aar (ADR-0012).
//
// Ownership boundary (ADR-0012 §4.2): this module carries NO capture policy.
// It is consent flow (RsacProjection), service plumbing (RsacCaptureService),
// the Java AudioRecord read loop (CaptureBridge), and name/PID→UID resolution
// helpers (PackageResolver). Target resolution, error classification, and
// stream semantics live in Rust (src/audio/android/).
//
// Source-complete per seed rsac-c4b8 and built by the mobile-android CI job.
// Runtime capture remains unverified until the Android device/emulator seed lands.

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
        // rsac-255b: instrumented frames-delivered evidence (mic path). For a
        // library module the androidTest APK is self-instrumenting — it is
        // both the instrumentation and the app-under-test, giving the test a
        // real uid that can hold RECORD_AUDIO (adb's shell uid cannot open an
        // AAudio input stream — PR #65 lesson).
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
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
    // The JNI surface is registered from librsac.so's JNI_OnLoad (rsac-77f1).
    // See README.md § Native library.
    //
    // rsac-255b: the TEST APK additionally carries librsac_ffi.so (the
    // shipped C ABI, cargo-ndk-built) + librsac_androidtest_shim.so (a
    // test-only JNI bridge, plain-NDK-clang-built from
    // src/androidTest/cpp/rsac_androidtest_shim.c) in
    // src/androidTest/jniLibs/x86_64/ — CI-generated, git-ignored, and
    // packaged into the androidTest APK ONLY (never the production AAR).
    // Deliberately NOT android.externalNativeBuild/CMake: that block is
    // module-global and would build the shim into every variant incl. the
    // AAR's jni/. See ci-android-emu.yml (the instrumented tier).

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

    // rsac-255b instrumented tier (test APK only). Versions expected; CI trues up.
    androidTestImplementation("androidx.test.ext:junit:1.2.1")
    androidTestImplementation("androidx.test:runner:1.6.2")
    androidTestImplementation("androidx.test:rules:1.6.1") // GrantPermissionRule
}
