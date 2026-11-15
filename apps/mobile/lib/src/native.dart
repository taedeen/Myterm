import 'dart:io' show Platform;

import 'package:flutter/services.dart';
import 'package:local_auth/local_auth.dart';

import 'rust/api.dart' show osProtect, osUnprotect;

/// A hardware-backed SSH key created in the Android Keystore.
class HardwareKey {
  HardwareKey({required this.alias, required this.publicSec1, required this.protection});

  final String alias;
  final Uint8List publicSec1;

  /// "StrongBox", "TEE" or "Software" (from Keystore KeyInfo).
  final String protection;

  static HardwareKey fromMap(Map<dynamic, dynamic> m) => HardwareKey(
        alias: m['alias'] as String,
        publicSec1: m['publicSec1'] as Uint8List,
        protection: m['protection'] as String,
      );
}

/// Platform features.
///
/// Android: Kotlin (MainActivity.kt) for Keystore keys, biometric prompts,
/// FLAG_SECURE and the session keep-alive service.
/// Windows: Windows Hello (local_auth) for confirmations, and DPAPI (Rust,
/// crates/ffi/src/dpapi.rs) to keep the vault key for Hello unlock. Hardware
/// SSH keys aren't available on Windows yet.
class Native {
  static const _ch = MethodChannel('dev.myterm/native');
  static final _auth = LocalAuthentication();

  static bool get _android => Platform.isAndroid;

  /// Hardware-bound SSH keys (Android Keystore). Not on Windows yet.
  static bool get hardwareKeysSupported => _android;

  /// What the biometric unlock is called here.
  static String get unlockName => Platform.isWindows ? 'Windows Hello' : 'biometrics';

  /// Windows Hello (face, fingerprint or PIN). False if cancelled or not set up.
  static Future<bool> _hello(String reason) async {
    try {
      return await _auth.authenticate(localizedReason: reason);
    } catch (_) {
      return false;
    }
  }

  /// Creates a P-256 signing key in StrongBox (falls back to TEE).
  static Future<HardwareKey> generateHardwareKey(String alias) async {
    if (!_android) throw UnsupportedError("Hardware keys aren't available on Windows yet.");
    return HardwareKey.fromMap(await _ch.invokeMethod('hwGenerate', {'alias': alias}));
  }

  static Future<List<HardwareKey>> listHardwareKeys() async {
    if (!_android) return [];
    final list = await _ch.invokeListMethod<Map<dynamic, dynamic>>('hwList') ?? [];
    return list.map(HardwareKey.fromMap).toList();
  }

  static Future<void> deleteHardwareKey(String alias) async {
    if (_android) await _ch.invokeMethod('hwDelete', {'alias': alias});
  }

  /// Shows the biometric prompt and returns a DER ECDSA signature,
  /// or an empty list if the user cancelled.
  static Future<Uint8List> signWithHardwareKey(String alias, Uint8List data) async {
    if (!_android) return Uint8List(0);
    try {
      return await _ch.invokeMethod<Uint8List>('hwSign', {'alias': alias, 'data': data}) ?? Uint8List(0);
    } on PlatformException {
      return Uint8List(0);
    }
  }

  /// Wraps the vault master key with a Keystore AES key (biometric prompt).
  static Future<Uint8List?> deviceWrap(Uint8List masterKey) async {
    if (Platform.isWindows) {
      if (!await _hello('Turn on Windows Hello unlock for MyTerm')) return null;
      return osProtect(data: masterKey);
    }
    return _ch.invokeMethod<Uint8List>('deviceWrap', {'mk': masterKey});
  }

  /// Unwraps the master key after the biometric prompt. Null if cancelled.
  static Future<Uint8List?> deviceUnwrap(Uint8List wrapped) async {
    if (Platform.isWindows) {
      if (!await _hello('Unlock MyTerm')) return null;
      try {
        return await osUnprotect(data: wrapped);
      } catch (_) {
        return null;
      }
    }
    try {
      return await _ch.invokeMethod<Uint8List>('deviceUnwrap', {'wrapped': wrapped});
    } on PlatformException {
      return null;
    }
  }

  /// Biometric or device-PIN confirmation without a key (Type password).
  static Future<bool> confirmUser(String title) async {
    if (Platform.isWindows) return _hello(title);
    return await _ch.invokeMethod<bool>('confirmUser', {'title': title}) ?? false;
  }

  /// FLAG_SECURE: blocks screenshots and screen recording of vault screens.
  static Future<void> setSecureWindow(bool secure) async {
    if (_android) await _ch.invokeMethod('setSecure', {'secure': secure});
  }

  /// Foreground service that keeps SSH sessions alive in the background.
  /// A name for this device in the sync device list, e.g. "Google Pixel 8"
  /// or the Windows computer name.
  static Future<String> deviceModel() async {
    if (_android) {
      try {
        return await _ch.invokeMethod<String>('deviceModel') ?? 'Android device';
      } catch (_) {
        return 'Android device';
      }
    }
    return Platform.localHostname;
  }

  static Future<void> keepAlive(int sessions) async {
    // Desktop apps keep running in the background anyway.
    if (_android) await _ch.invokeMethod('keepAlive', {'sessions': sessions});
  }
}

/// Overwrites a byte list in place (best effort; Dart may have copies).
/// Lists from platform channels can be read-only views; those are skipped
/// rather than throwing.
void wipe(Uint8List? bytes) {
  if (bytes == null) return;
  try {
    bytes.fillRange(0, bytes.length, 0);
  } on UnsupportedError {
    // Unmodifiable view: nothing we can overwrite from Dart.
  }
}
