import 'package:flutter/services.dart';

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

/// Calls into Kotlin (MainActivity.kt): Keystore keys, biometric prompts,
/// FLAG_SECURE and the session keep-alive service.
class Native {
  static const _ch = MethodChannel('dev.myterm/native');

  /// Creates a P-256 signing key in StrongBox (falls back to TEE).
  static Future<HardwareKey> generateHardwareKey(String alias) async =>
      HardwareKey.fromMap(await _ch.invokeMethod('hwGenerate', {'alias': alias}));

  static Future<List<HardwareKey>> listHardwareKeys() async {
    final list = await _ch.invokeListMethod<Map<dynamic, dynamic>>('hwList') ?? [];
    return list.map(HardwareKey.fromMap).toList();
  }

  static Future<void> deleteHardwareKey(String alias) => _ch.invokeMethod('hwDelete', {'alias': alias});

  /// Shows the biometric prompt and returns a DER ECDSA signature,
  /// or an empty list if the user cancelled.
  static Future<Uint8List> signWithHardwareKey(String alias, Uint8List data) async {
    try {
      return await _ch.invokeMethod<Uint8List>('hwSign', {'alias': alias, 'data': data}) ?? Uint8List(0);
    } on PlatformException {
      return Uint8List(0);
    }
  }

  /// Wraps the vault master key with a Keystore AES key (biometric prompt).
  static Future<Uint8List?> deviceWrap(Uint8List masterKey) =>
      _ch.invokeMethod<Uint8List>('deviceWrap', {'mk': masterKey});

  /// Unwraps the master key after the biometric prompt. Null if cancelled.
  static Future<Uint8List?> deviceUnwrap(Uint8List wrapped) async {
    try {
      return await _ch.invokeMethod<Uint8List>('deviceUnwrap', {'wrapped': wrapped});
    } on PlatformException {
      return null;
    }
  }

  /// Biometric or device-PIN confirmation without a key (Type password).
  static Future<bool> confirmUser(String title) async =>
      await _ch.invokeMethod<bool>('confirmUser', {'title': title}) ?? false;

  /// FLAG_SECURE: blocks screenshots and screen recording of vault screens.
  static Future<void> setSecureWindow(bool secure) => _ch.invokeMethod('setSecure', {'secure': secure});

  /// Foreground service that keeps SSH sessions alive in the background.
  static Future<void> keepAlive(int sessions) => _ch.invokeMethod('keepAlive', {'sessions': sessions});
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
