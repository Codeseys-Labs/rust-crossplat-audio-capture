#!/bin/bash

# Download a sample from Internet Archive
echo "Downloading test audio MP3 from Internet Archive..."
curl -L "https://ia800901.us.archive.org/23/items/gd70-02-14.early-late.sbd.cotsman.18115.sbeok.shnf/gd70-02-14d1t02.mp3" -o "test_audio.mp3"

echo "Test audio MP3 downloaded as test_audio.mp3"
