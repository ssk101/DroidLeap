#!/bin/bash
# Cross-compile tablet-edge.exe for Windows (x86_64) with Zig, from Linux,
# macOS or Windows. Override the compiler with ZIG=/path/to/zig.
set -euo pipefail
cd "$(dirname "$0")"

ZIG=${ZIG:-zig}
if ! command -v "$ZIG" >/dev/null; then
    echo "error: '$ZIG' not found. Install Zig 0.17 or later (https://ziglang.org/download/)" \
         "or set ZIG=/path/to/zig." >&2
    exit 1
fi

"$ZIG" cc -target x86_64-windows-gnu -O2 -s \
    -Wall -Wextra -Werror -Wno-missing-field-initializers \
    -o tablet-edge.exe tablet-edge.c tablet-edge.rc \
    -Wl,--subsystem,windows \
    -luser32 -lshell32 -lcomdlg32 -lgdi32

ls -l tablet-edge.exe
