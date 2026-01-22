#!/bin/bash
set -e

# Create venv if not exists
if [ ! -d ".venv" ]; then
    echo "Creating virtual environment..."
    python3 -m venv .venv
fi

# Activate
source .venv/bin/activate

# Install pyserial
echo "Installing pyserial..."
pip install pyserial

# Run script
echo "Running verification..."
python3 scripts/verify_python.py
