import 'dart:convert';
import 'dart:ffi' as ffi;

import 'package:ffi/ffi.dart';
import 'package:flutter/services.dart';

import '../connection/native_library.dart';

typedef _Connect = int Function(
  ffi.Pointer<ffi.Uint8>,
  int,
  int,
  ffi.Pointer<ffi.Uint8>,
  int,
  ffi.Pointer<ffi.Uint8>,
  int,
);

final class NativePreview {
  NativePreview._(ffi.DynamicLibrary library, this._libraryPath)
    : _connect = library
          .lookupFunction<
            ffi.Uint64 Function(
              ffi.Pointer<ffi.Uint8>,
              ffi.Size,
              ffi.Uint16,
              ffi.Pointer<ffi.Uint8>,
              ffi.Size,
              ffi.Pointer<ffi.Uint8>,
              ffi.Size,
            ),
            _Connect
          >('nexdesk_m1_connect'),
      _disconnect = library
          .lookupFunction<ffi.Void Function(ffi.Uint64), void Function(int)>(
            'nexdesk_m1_disconnect',
          ),
      _status = library
          .lookupFunction<ffi.Uint32 Function(ffi.Uint64), int Function(int)>(
            'nexdesk_m1_status',
          ),
      _size = _counter(library, 'nexdesk_m1_frame_size'),
      _presented = _counter(library, 'nexdesk_m1_presented_frames'),
      _textureWait = _counter(library, 'nexdesk_m1_texture_wait_us');

  static const _textureChannel = MethodChannel('dev.nexdesk/m1_texture');

  final String _libraryPath;
  String get libraryPath => _libraryPath;
  final _Connect _connect;
  final void Function(int) _disconnect;
  final int Function(int) _status, _size, _presented, _textureWait;

  static NativePreview? tryLoad() {
    final path = locateCoreLibrary();
    if (path == null) return null;
    try {
      return NativePreview._(ffi.DynamicLibrary.open(path), path);
    } on ArgumentError {
      return null;
    }
  }

  int connect(String address, int port, String code, String identity) {
    final addressBytes = utf8.encode(address);
    final codeBytes = utf8.encode(code);
    final identityBytes = utf8.encode(identity);
    if (port < 1 ||
        port > 65535 ||
        addressBytes.length > 128 ||
        codeBytes.length != 6 ||
        identityBytes.length > 128) {
      return 0;
    }
    return using((arena) {
      final nativeCode = _text(arena, codeBytes);
      try {
        return _connect(
          _text(arena, addressBytes),
          addressBytes.length,
          port,
          nativeCode,
          codeBytes.length,
          _text(arena, identityBytes),
          identityBytes.length,
        );
      } finally {
        nativeCode
            .asTypedList(codeBytes.length)
            .fillRange(0, codeBytes.length, 0);
        codeBytes.fillRange(0, codeBytes.length, 0);
      }
    });
  }

  Future<int?> attachTexture(int viewer) => _textureChannel.invokeMethod<int>(
    'attach',
    {'viewer': viewer, 'library': _libraryPath},
  );

  Future<void> setFullscreen(bool enabled) =>
      _textureChannel.invokeMethod<void>('fullscreen', enabled);

  static Future<String?> selectDirectory() =>
      _textureChannel.invokeMethod<String>('selectDirectory');

  Future<void> stop(int viewer) async {
    _disconnect(viewer);
    try {
      await _textureChannel.invokeMethod<void>('detach', viewer);
    } on PlatformException {
      // The native session is already closed, including when the runner exits.
    } on MissingPluginException {
      // There is no texture when the preview runner is unavailable.
    }
  }

  PreviewState inspect(int viewer) {
    final size = _size(viewer);
    return PreviewState(
      status: _status(viewer),
      width: size >>> 32,
      height: size & 0xffffffff,
      presented: _presented(viewer),
      textureWaitUs: _textureWait(viewer),
    );
  }

  static int Function(int) _counter(ffi.DynamicLibrary library, String name) =>
      library
          .lookupFunction<ffi.Uint64 Function(ffi.Uint64), int Function(int)>(
            name,
          );

  static ffi.Pointer<ffi.Uint8> _text(Arena arena, List<int> bytes) {
    if (bytes.isEmpty) return ffi.nullptr;
    final buffer = arena<ffi.Uint8>(bytes.length);
    buffer.asTypedList(bytes.length).setAll(0, bytes);
    return buffer;
  }
}

final class PreviewState {
  const PreviewState({
    this.status = 0,
    this.width = 0,
    this.height = 0,
    this.presented = 0,
    this.textureWaitUs = 0,
  });

  final int status, width, height, presented, textureWaitUs;
}
