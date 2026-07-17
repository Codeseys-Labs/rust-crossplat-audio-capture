package rsac_test

import (
	"context"
	"fmt"
	"time"

	rsac "github.com/Codeseys-Labs/rust-crossplat-audio-capture/bindings/rsac-go"
)

// ExampleNewCaptureBuilder demonstrates the basic capture workflow:
// build a capture, start it, stream audio, then clean up.
func ExampleNewCaptureBuilder() {
	capture, err := rsac.NewCaptureBuilder().
		WithSystemDefault().
		SampleRate(48000).
		Channels(2).
		Build()
	if err != nil {
		fmt.Printf("build error: %v\n", err)
		return
	}
	defer capture.Close()

	if err := capture.Start(); err != nil {
		fmt.Printf("start error: %v\n", err)
		return
	}

	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()

	count := 0
	for buf := range capture.Stream(ctx) {
		count++
		_ = buf.Data() // process audio data
		if count >= 10 {
			cancel()
		}
	}
	fmt.Printf("received %d buffers\n", count)
}

// ExampleNewCaptureBuilder_applicationByName captures audio from a specific
// application matched by name.
func ExampleNewCaptureBuilder_applicationByName() {
	capture, err := rsac.NewCaptureBuilder().
		WithApplicationByName("Firefox").
		SampleRate(48000).
		Build()
	if err != nil {
		fmt.Printf("error: %v\n", err)
		return
	}
	defer capture.Close()

	if err := capture.Start(); err != nil {
		fmt.Printf("start error: %v\n", err)
		return
	}

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	for buf := range capture.Stream(ctx) {
		fmt.Printf("%d frames @ %d Hz\n", buf.NumFrames(), buf.SampleRate())
	}
}

// ExampleNewCaptureBuilder_processTree captures audio from a process tree.
func ExampleNewCaptureBuilder_processTree() {
	capture, err := rsac.NewCaptureBuilder().
		WithProcessTree(1234).
		SampleRate(44100).
		Channels(1).
		Build()
	if err != nil {
		fmt.Printf("error: %v\n", err)
		return
	}
	defer capture.Close()

	_ = capture.Start()

	// Non-blocking read loop.
	for i := 0; i < 100; i++ {
		buf, ok, err := capture.TryReadBuffer()
		if err != nil {
			fmt.Printf("read error: %v\n", err)
			break
		}
		if ok {
			fmt.Printf("got %d frames\n", buf.NumFrames())
		}
	}
}

// ExampleNewCaptureBuilder_blocking demonstrates blocking reads.
func ExampleNewCaptureBuilder_blocking() {
	capture, err := rsac.NewCaptureBuilder().
		WithSystemDefault().
		SampleRate(48000).
		Build()
	if err != nil {
		fmt.Printf("error: %v\n", err)
		return
	}
	defer capture.Close()

	if err := capture.Start(); err != nil {
		fmt.Printf("start error: %v\n", err)
		return
	}

	// Blocking read — waits until audio data is available.
	buf, err := capture.ReadBuffer()
	if err != nil {
		fmt.Printf("read error: %v\n", err)
		return
	}
	fmt.Printf("got %d frames, %d channels @ %d Hz\n",
		buf.NumFrames(), buf.Channels(), buf.SampleRate())
}

// ExampleAudioCapture_Stream shows the recommended way to consume audio:
// via a Go channel with context cancellation.
func ExampleAudioCapture_Stream() {
	capture, err := rsac.NewCaptureBuilder().
		WithSystemDefault().
		SampleRate(48000).
		Channels(2).
		Build()
	if err != nil {
		fmt.Printf("error: %v\n", err)
		return
	}
	defer capture.Close()
	_ = capture.Start()

	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()

	total := 0
	for buf := range capture.Stream(ctx) {
		total += buf.NumFrames()
	}
	fmt.Printf("total frames: %d\n", total)
}

// ExampleAudioCapture_StreamWithErrors shows how to handle errors in the stream.
func ExampleAudioCapture_StreamWithErrors() {
	capture, err := rsac.NewCaptureBuilder().
		WithSystemDefault().
		SampleRate(48000).
		Build()
	if err != nil {
		fmt.Printf("error: %v\n", err)
		return
	}
	defer capture.Close()
	_ = capture.Start()

	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()

	for result := range capture.StreamWithErrors(ctx) {
		if result.Err != nil {
			fmt.Printf("stream error: %v\n", result.Err)
			break
		}
		fmt.Printf("%d frames\n", result.Buffer.NumFrames())
	}
}

// ExampleAudioCapture_SetCallback demonstrates push-based audio delivery.
func ExampleAudioCapture_SetCallback() {
	capture, err := rsac.NewCaptureBuilder().
		WithSystemDefault().
		SampleRate(48000).
		Build()
	if err != nil {
		fmt.Printf("error: %v\n", err)
		return
	}
	defer capture.Close()

	// Set callback BEFORE starting capture.
	err = capture.SetCallback(func(buf rsac.AudioBuffer) {
		fmt.Printf("callback: %d frames\n", buf.NumFrames())
	})
	if err != nil {
		fmt.Printf("callback error: %v\n", err)
		return
	}

	_ = capture.Start()
	time.Sleep(2 * time.Second)
	_ = capture.Stop()
}

// ExampleAudioCapture_OverrunCount shows how to monitor for buffer overruns.
func ExampleAudioCapture_OverrunCount() {
	capture, err := rsac.NewCaptureBuilder().
		WithSystemDefault().
		SampleRate(48000).
		Build()
	if err != nil {
		fmt.Printf("error: %v\n", err)
		return
	}
	defer capture.Close()
	_ = capture.Start()

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	for buf := range capture.Stream(ctx) {
		_ = buf.Data()
		if dropped := capture.OverrunCount(); dropped > 0 {
			fmt.Printf("warning: %d buffers dropped (consumer too slow)\n", dropped)
		}
	}
}

// ExamplePlatformCapabilities shows how to query platform audio capabilities.
func ExamplePlatformCapabilities() {
	caps, err := rsac.PlatformCapabilities()
	if err != nil {
		fmt.Printf("error: %v\n", err)
		return
	}
	fmt.Printf("Backend: %s\n", caps.BackendName)
	fmt.Printf("System capture: %v\n", caps.SupportsSystemCapture)
	fmt.Printf("App capture: %v\n", caps.SupportsAppCapture)
	fmt.Printf("Process tree: %v\n", caps.SupportsProcessTree)
	fmt.Printf("Device selection: %v\n", caps.SupportsDeviceSelection)
	fmt.Printf("Device change notifications: %v\n", caps.SupportsDeviceChangeNotifications)
	fmt.Printf("Requires user consent: %v\n", caps.RequiresUserConsent)
	fmt.Printf("Max channels: %d\n", caps.MaxChannels)
	fmt.Printf("Sample formats: %v\n", caps.SupportedSampleFormats)
	fmt.Printf("Sample rate range: %d-%d Hz\n", caps.MinSampleRate, caps.MaxSampleRate)
	fmt.Printf("Config-time rate whitelist: %v\n", caps.SupportedSampleRates)
}

// ExampleListDevices demonstrates audio device enumeration.
func ExampleListDevices() {
	devices, err := rsac.ListDevices()
	if err != nil {
		fmt.Printf("error: %v\n", err)
		return
	}
	for _, dev := range devices {
		defaultStr := ""
		if dev.IsDefault {
			defaultStr = " (default)"
		}
		fmt.Printf("  %s: %s%s\n", dev.ID, dev.Name, defaultStr)
	}
}

// ExampleComposition demonstrates multi-source channel composition: build a
// composition from named groups of capture sources, start it, and read composed
// buffers. The composition mixes each group down according to its layout and
// appends the groups' channels into one interleaved stream.
func ExampleComposition() {
	// A "voice" group folding two app sources to mono, plus a "system" group
	// keeping the system mix's native channels.
	voice, err := rsac.NewGroup("voice")
	if err != nil {
		fmt.Printf("group error: %v\n", err)
		return
	}
	_ = voice.SetLayout(rsac.LayoutMono)
	_ = voice.AddSource("name:Discord")
	_ = voice.AddSourceWithGain("name:Zoom", 0.8)

	system, err := rsac.NewGroup("system")
	if err != nil {
		fmt.Printf("group error: %v\n", err)
		return
	}
	_ = system.SetLayout(rsac.LayoutStereo)
	_ = system.AddSource("system")

	builder, err := rsac.NewCompositionBuilder()
	if err != nil {
		fmt.Printf("builder error: %v\n", err)
		return
	}
	_ = builder.SetSampleRate(48000)
	if err := builder.AddGroup(voice); err != nil {
		fmt.Printf("add voice: %v\n", err)
		return
	}
	if err := builder.AddGroup(system); err != nil {
		fmt.Printf("add system: %v\n", err)
		return
	}

	comp, err := builder.Build()
	if err != nil {
		fmt.Printf("build error: %v\n", err)
		return
	}
	defer comp.Close()

	if err := comp.Start(); err != nil {
		fmt.Printf("start error: %v\n", err)
		return
	}

	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()

	for buf := range comp.Stream(ctx) {
		fmt.Printf("%d frames, %d composed channels\n", buf.NumFrames(), buf.Channels())
	}
}

// ExampleDefaultDevice shows how to get the default device.
func ExampleDefaultDevice() {
	dev, err := rsac.DefaultDevice(rsac.DeviceOutput)
	if err != nil {
		fmt.Printf("error: %v\n", err)
		return
	}
	fmt.Printf("Default output: %s (%s)\n", dev.Name, dev.ID)
}

// ExampleAudioCapture_Close demonstrates idempotent close behavior.
func ExampleAudioCapture_Close() {
	capture, err := rsac.NewCaptureBuilder().
		WithSystemDefault().
		Build()
	if err != nil {
		fmt.Printf("error: %v\n", err)
		return
	}

	// Close is idempotent — safe to call multiple times.
	_ = capture.Close()
	_ = capture.Close()
	_ = capture.Close()
	fmt.Println("closed successfully")
	// Output: closed successfully
}
