import 'dart:convert';

import 'package:flutter/services.dart';

import '../connection/desktop_settings.dart';

final class SessionWindowLaunch {
  const SessionWindowLaunch({
    required this.viewer,
    required this.address,
    required this.port,
    required this.origin,
    required this.settings,
    required this.saved,
  });

  static const argument = '--nexdesk-session-window';

  factory SessionWindowLaunch.decode(String argument) {
    final data = jsonDecode(argument) as Map<String, Object?>;
    return SessionWindowLaunch(
      viewer: data['viewer'] as int,
      address: data['address'] as String,
      port: data['port'] as int,
      origin: data['origin'] as String?,
      settings: DesktopSettings.fromJson(
        data['settings'] as Map<String, Object?>,
      ),
      saved: data['saved'] == true,
    );
  }

  final int viewer, port;
  final String address;
  final String? origin;
  final DesktopSettings settings;
  final bool saved;

  String get label =>
      origin == null ? LanTarget(address, port.toString()).endpoint : address;

  String encode() => jsonEncode({
    'viewer': viewer,
    'address': address,
    'port': port,
    'origin': origin,
    'settings': settings.toJson(),
    'saved': saved,
  });
}

abstract final class SessionWindow {
  static const _channel = MethodChannel('dev.nexdesk/session_window');

  static Future<bool> open(SessionWindowLaunch launch, String title) =>
      _operation('open', {
        'id': launch.viewer,
        'title': title,
        'launch': launch.encode(),
      });

  static Future<bool> focus(int viewer) => _operation('focus', viewer);

  static Future<bool> requestClose(int viewer) =>
      _operation('requestClose', viewer);

  static Future<bool> close() => _operation('close');

  static void onClosed(void Function(int)? callback) {
    _channel.setMethodCallHandler(
      callback == null
          ? null
          : (call) async {
              if (call.method == 'closed' && call.arguments is int) {
                callback(call.arguments as int);
              }
            },
    );
  }

  static void onCloseRequested(Future<void> Function()? callback) {
    _channel.setMethodCallHandler(
      callback == null
          ? null
          : (call) async {
              if (call.method == 'requestClose') await callback();
            },
    );
  }

  static Future<bool> _operation(String method, [Object? arguments]) async {
    try {
      return await _channel.invokeMethod<bool>(method, arguments) == true;
    } on PlatformException {
      return false;
    } on MissingPluginException {
      return false;
    }
  }
}
