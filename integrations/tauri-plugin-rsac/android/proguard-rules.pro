# Consumer ProGuard rules for tauri-plugin-rsac.
# The Tauri annotation processor discovers @TauriPlugin/@Command via reflection;
# keep the plugin class and its command methods.
-keep class ai.codeseys.rsac.tauri.** { *; }
