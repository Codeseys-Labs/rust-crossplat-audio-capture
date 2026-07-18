// tauri-plugin-rsac Android — Gradle settings.
//
// The Tauri Android build harness injects the `:tauri-android` project
// (its API library) at `.tauri/tauri-api` when the plugin is built inside a
// Tauri app. This file mirrors the upstream plugin convention so a standalone
// or CLI-driven build can resolve it. Compile-proof / source-shipped: a full
// Gradle assemble is deferred to the runtime lane (rsac-e6d3/rsac-97c8).

pluginManagement {
    repositories {
        google {
            content {
                includeGroupByRegex("com\\.android.*")
                includeGroupByRegex("com\\.google.*")
                includeGroupByRegex("androidx.*")
            }
        }
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral()
    }
}

rootProject.name = "tauri-plugin-rsac"

// Injected by the Tauri CLI when building inside an app; the path mirrors the
// upstream plugins-workspace convention.
// include(":tauri-android")
// project(":tauri-android").projectDir = File("./.tauri/tauri-api")
