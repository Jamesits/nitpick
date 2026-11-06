# Plan: Build System

## Windows

### 1. Clean Install Test

Test if the package works on a Windows device without the development environment.

## Linux

### 3. Clean Install Test

Test if the package works on a Linux device without the development environment.

## macOS

### 4. Build and run on macOS

Run the launcher with Homebrew Qt 6.12. Verify on an HDR display:
- The HDR10 pattern shows without an error, also with HDR10 metadata.
- The pattern fills the safe area and the status line has no size warning.
- The reference table size equals the pattern size in the status line.

### 5. Clean Install Test

Test if the package (`cargo make package`) works on a macOS device without the development environment. Verify that Gatekeeper allows the ad-hoc signed bundle after the user approves it, and that the HDR10 pattern shows.
