// index.js — platform-specific native binary loader for @rsac/audio.
//
// napi-rs generates a per-platform `.node` binary named using the `napi.name`
// field from package.json combined with the target triple (e.g. rsac-audio.darwin-arm64.node).
// This file resolves and requires the correct binary for the host platform.

const { existsSync } = require('fs')
const { join } = require('path')
const { platform, arch } = process

let nativeBinding = null
let localFileExisted = false
let loadError = null

function isMusl() {
  // Node 18+: process.report.getReport().header.glibcVersionRuntime is set on glibc.
  if (typeof process.report !== 'undefined' && typeof process.report.getReport === 'function') {
    const report = process.report.getReport()
    const { glibcVersionRuntime } = report.header
    return !glibcVersionRuntime
  }
  // Fallback: assume musl if we can't determine otherwise.
  return true
}

switch (platform) {
  case 'win32':
    switch (arch) {
      case 'x64':
        localFileExisted = existsSync(join(__dirname, 'rsac-audio.win32-x64-msvc.node'))
        try {
          if (localFileExisted) {
            nativeBinding = require('./rsac-audio.win32-x64-msvc.node')
          } else {
            nativeBinding = require('@rsac/audio-win32-x64-msvc')
          }
        } catch (e) {
          loadError = e
        }
        break
      case 'arm64':
        localFileExisted = existsSync(join(__dirname, 'rsac-audio.win32-arm64-msvc.node'))
        try {
          if (localFileExisted) {
            nativeBinding = require('./rsac-audio.win32-arm64-msvc.node')
          } else {
            nativeBinding = require('@rsac/audio-win32-arm64-msvc')
          }
        } catch (e) {
          loadError = e
        }
        break
      default:
        throw new Error(`Unsupported architecture on Windows: ${arch}`)
    }
    break
  case 'darwin':
    switch (arch) {
      case 'x64':
        localFileExisted = existsSync(join(__dirname, 'rsac-audio.darwin-x64.node'))
        try {
          if (localFileExisted) {
            nativeBinding = require('./rsac-audio.darwin-x64.node')
          } else {
            nativeBinding = require('@rsac/audio-darwin-x64')
          }
        } catch (e) {
          loadError = e
        }
        break
      case 'arm64':
        localFileExisted = existsSync(join(__dirname, 'rsac-audio.darwin-arm64.node'))
        try {
          if (localFileExisted) {
            nativeBinding = require('./rsac-audio.darwin-arm64.node')
          } else {
            nativeBinding = require('@rsac/audio-darwin-arm64')
          }
        } catch (e) {
          loadError = e
        }
        break
      default:
        throw new Error(`Unsupported architecture on macOS: ${arch}`)
    }
    break
  case 'linux':
    switch (arch) {
      case 'x64':
        if (isMusl()) {
          localFileExisted = existsSync(join(__dirname, 'rsac-audio.linux-x64-musl.node'))
          try {
            if (localFileExisted) {
              nativeBinding = require('./rsac-audio.linux-x64-musl.node')
            } else {
              nativeBinding = require('@rsac/audio-linux-x64-musl')
            }
          } catch (e) {
            loadError = e
          }
        } else {
          localFileExisted = existsSync(join(__dirname, 'rsac-audio.linux-x64-gnu.node'))
          try {
            if (localFileExisted) {
              nativeBinding = require('./rsac-audio.linux-x64-gnu.node')
            } else {
              nativeBinding = require('@rsac/audio-linux-x64-gnu')
            }
          } catch (e) {
            loadError = e
          }
        }
        break
      case 'arm64':
        if (isMusl()) {
          localFileExisted = existsSync(join(__dirname, 'rsac-audio.linux-arm64-musl.node'))
          try {
            if (localFileExisted) {
              nativeBinding = require('./rsac-audio.linux-arm64-musl.node')
            } else {
              nativeBinding = require('@rsac/audio-linux-arm64-musl')
            }
          } catch (e) {
            loadError = e
          }
        } else {
          localFileExisted = existsSync(join(__dirname, 'rsac-audio.linux-arm64-gnu.node'))
          try {
            if (localFileExisted) {
              nativeBinding = require('./rsac-audio.linux-arm64-gnu.node')
            } else {
              nativeBinding = require('@rsac/audio-linux-arm64-gnu')
            }
          } catch (e) {
            loadError = e
          }
        }
        break
      default:
        throw new Error(`Unsupported architecture on Linux: ${arch}`)
    }
    break
  default:
    throw new Error(`Unsupported OS: ${platform}, architecture: ${arch}`)
}

if (!nativeBinding) {
  if (loadError) {
    throw loadError
  }
  throw new Error(`Failed to load native binding for @rsac/audio on ${platform}-${arch}`)
}

const {
  AudioCapture,
  CaptureTarget,
  listDevices,
  getDefaultDevice,
  platformCapabilities,
} = nativeBinding

module.exports.AudioCapture = AudioCapture
module.exports.CaptureTarget = CaptureTarget
module.exports.listDevices = listDevices
module.exports.getDefaultDevice = getDefaultDevice
module.exports.platformCapabilities = platformCapabilities
