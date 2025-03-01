FROM sickcodes/docker-osx:latest

# Install Homebrew
RUN /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"

# Install dependencies
RUN brew install \
    rust \
    blackhole-2ch \
    vlc \
    git \
    curl

# Create test audio directory
RUN mkdir -p /test_audio

# Configure audio permissions
RUN sudo sqlite3 "$HOME/Library/Application Support/com.apple.TCC/TCC.db" \
    "INSERT OR REPLACE INTO access VALUES('kTCCServiceMicrophone','com.apple.audio.AudioMIDISetup',0,2,4,1,NULL,NULL,0,'UNUSED',NULL,0,1);"

# Copy test script
COPY test.sh /test.sh
RUN chmod +x /test.sh

WORKDIR /app

CMD ["/test.sh"] 