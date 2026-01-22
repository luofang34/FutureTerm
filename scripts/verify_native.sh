#!/bin/bash
PORT="/dev/cu.usbmodemDAP_Ridiar3"
BAUD="1500000"

echo "======================================"
echo "Native Serial Verification Tool"
echo "Port: $PORT"
echo "Baud: $BAUD"
echo "======================================"
echo "Configuring port (8N1)..."

# Configure port: 1.5M baud, 8 bits, No parity, 1 stop bit, No hardware flow control, Raw input
stty -f "$PORT" "$BAUD" cs8 -parenb -cstopb -crtscts raw

echo "Reading from port. Please type 'reboot' on the device or reset it."
echo "Press Ctrl+C to exit."
echo "--------------------------------------"

# Start reading in background
cat "$PORT" &
CAT_PID=$!

# Wait a moment for cat to hook up
sleep 1

# Send reboot command
echo "Sending reboot command..."
echo "sudo reboot" > "$PORT"

# Wait for output
wait $CAT_PID
