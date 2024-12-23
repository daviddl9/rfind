#!/bin/bash

# Build and install the binary
# cargo build --release
# sudo cp target/release/rfind /usr/local/bin/

# Build initial index
rfind --rebuild

# Set up system service based on platform
if [[ "$OSTYPE" == "darwin"* ]]; then
    # macOS
    cat > ~/Library/LaunchAgents/com.rfind.watcher.plist << EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.rfind.watcher</string>
    <key>ProgramArguments</key>
    <array>
        <string>/usr/local/bin/rfind</string>
        <string>--daemon</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
</dict>
</plist>
EOF
    launchctl load ~/Library/LaunchAgents/com.rfind.watcher.plist

elif [[ "$OSTYPE" == "linux-gnu"* ]]; then
    # Linux (systemd)
    sudo cat > /etc/systemd/system/rfind-watcher.service << EOF
[Unit]
Description=RFind File System Watcher
After=network.target

[Service]
ExecStart=/usr/local/bin/rfind --daemon
Restart=always
User=$USER

[Install]
WantedBy=multi-user.target
EOF
    
    sudo systemctl daemon-reload
    sudo systemctl enable rfind-watcher
    sudo systemctl start rfind-watcher
fi

echo "Installation complete! The file system watcher is now running."