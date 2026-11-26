import 'dart:convert';
import 'dart:io';

import 'package:flutter/foundation.dart' show FlutterError;
import 'package:flutter/services.dart';

Future<String?> loadDesktopVersion() async {
  try {
    final String? version;
    if (Platform.isLinux) {
      final data = jsonDecode(await rootBundle.loadString('version.json'));
      version = data is Map && data['version'] is String
          ? data['version'] as String
          : null;
    } else {
      version = await const MethodChannel('dev.nexdesk/background')
          .invokeMethod<String>('appVersion');
    }
    if (version != null && version.trim().isNotEmpty) return version.trim();
  } on MissingPluginException {
    // A developer runner may not include the native metadata bridge.
  } on PlatformException {
    // Use the release build's metadata if the platform could not supply it.
  } on FlutterError {
    // Linux debug bundles can be created without a generated version asset.
  } on FormatException {
    // Ignore a malformed generated asset.
  }
  const fallback = String.fromEnvironment('NEXDESK_RELEASE_VERSION');
  return fallback.isEmpty ? null : fallback;
}
