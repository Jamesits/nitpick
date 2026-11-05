# Plan: Build System

## Windows

### 1. Clean Install Test

Test if the package works on a Windows device without the development environment.

## Linux

### 2. Build and run on Linux

Build on a distribution with Qt 6.12 private headers and Vulkan headers. Fix compile errors in `pattern_window.cpp`.

### 3. Clean Install Test

Test if the package works on a Linux device without the development environment.

## macOS

### 4. Build and run on macOS

Build with Qt 6.12 and Xcode. Fix compile errors in `pattern_window.cpp` and `metal_hdr.mm`.

### 5. Clean Install Test

Test if the package works on a macOS device without the development environment.
