#!/bin/bash

# Create test audio directory if it doesn't exist
mkdir -p test_audio

# Download a sample from Internet Archive
echo "Downloading test audio from Internet Archive..."
curl -L "https://archive.org/download/gd70-02-14.early-late.sbd.cotsman.18115.sbeok.shnf/gd70-02-14d1t01.shn" -o "test_audio/sample.shn"

# Convert to WAV format using ffmpeg
echo "Converting to WAV format..."
ffmpeg -i "test_audio/sample.shn" -ar 48000 -ac 2 -acodec pcm_f32le "test_audio/sample.wav"

echo "Test audio prepared successfully!"