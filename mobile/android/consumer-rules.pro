# rsac consumer ProGuard/R8 rules — shipped inside the AAR and applied to
# host apps automatically.
#
# The Rust side (src/audio/android/jni.rs, rsac-77f1) reaches this glue via
# JNI FindClass / GetMethodID and RegisterNatives, which R8 cannot see.
# Nothing under ai.codeseys.rsac may be renamed or stripped.
-keep class ai.codeseys.rsac.** { *; }

# Keep native-method names bindable (RegisterNatives matches by name+sig).
-keepclasseswithmembers class ai.codeseys.rsac.** {
    native <methods>;
}
