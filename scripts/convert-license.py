#!/usr/bin/env python3
import re

# Path to your exported .reg file
reg_file_path = "export.reg"
output_file_path = "licence.YOUR_SERVER_IP"  # Replace with target server IP/hostname

with open(reg_file_path, "r", encoding="utf-16-le", errors="ignore") as f:
    content = f.read()

# Fallback to UTF-8 if utf-16 fails or file is plain ASCII
if '"ClientLicense"' not in content:
    with open(reg_file_path, "r", encoding="utf-8", errors="ignore") as f:
        content = f.read()

# Extract the hex block for ClientLicense
match = re.search(r'"ClientLicense"=hex:(.*?)(?:\r?\n\r?\n|\Z)', content, re.DOTALL)
if not match:
    raise ValueError("ClientLicense hex block not found in the registry file.")

# Clean up backslashes, newlines, commas, and spaces
hex_data = match.group(1)
hex_clean = re.sub(r'[\s\\\r\n,]', '', hex_data)

# Convert hex string to binary bytes and save
binary_data = bytes.fromhex(hex_clean)
with open(output_file_path, "wb") as out:
    out.write(binary_data)

print(f"Successfully generated rdesktop license file: {output_file_path}")
