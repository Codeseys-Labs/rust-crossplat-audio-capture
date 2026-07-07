// rsac Android glue — Gradle settings (single-module Android library build).
//
// Source-complete per seed rsac-c4b8; NOT yet built in CI (rsac-1a6e adds the
// Gradle CI job and the wrapper). Version pins below are "expected; CI trues up".
//
// The Gradle wrapper (gradlew + gradle-wrapper.jar) is deliberately absent:
// no Gradle exists on the authoring machine and we do not hand-craft binary
// jars. rsac-1a6e generates and commits the wrapper (`gradle wrapper
// --gradle-version 8.11.1` expected; CI trues up).

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

rootProject.name = "rsac-android"
