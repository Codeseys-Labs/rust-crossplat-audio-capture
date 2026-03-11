// TODO: Rewrite to use new API (AudioCaptureBuilder)
// This example previously demonstrated two old APIs:
//   1. get_audio_backend() / AudioCaptureBackend trait
//   2. ProcessAudioCapture (legacy Windows-only API)
// Both have been removed. This example needs to be rewritten using:
//   rsac::AudioCaptureBuilder::new()
//       .with_target(CaptureTarget::Application(...))
//       .build()? -> AudioCapture -> .start()? -> CapturingStream

#[cfg(target_os = "windows")]
fn main() {
    println!("This example needs to be rewritten to use the new AudioCaptureBuilder API.");
    println!("See docs/architecture/API_DESIGN.md for the new API surface.");

    // TODO: Rewrite Example 1 — was: get_audio_backend() -> backend.list_applications()
    //       -> backend.capture_application(app, config) -> stream.start()/read()/stop()
    //
    // TODO: Rewrite Example 2 — was: ProcessAudioCapture::new() -> capture.init_for_process()
    //       -> capture.start() -> capture.get_data() -> capture.stop()
}

#[cfg(not(target_os = "windows"))]
fn main() {
    println!("This example only works on Windows");
}
