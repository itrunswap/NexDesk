import 'dart:convert';
import 'dart:ffi' as ffi;

import 'package:ffi/ffi.dart';

import 'native_library.dart';

enum TargetIssue { deviceId, address, port, text, nativeFailure }

abstract interface class TargetValidator {
  int get defaultLanPort;

  TargetIssue? checkDeviceId(String id);

  TargetIssue? checkLan(String address, String port);
}

typedef _DeviceIdCheck = int Function(ffi.Pointer<ffi.Uint8>, int);
typedef _LanCheck = int Function(
  ffi.Pointer<ffi.Uint8>,
  int,
  ffi.Pointer<ffi.Uint8>,
  int,
);

/// The native library is selected at build time for the development preview.
final class RustTargetValidator implements TargetValidator {
  RustTargetValidator._(ffi.DynamicLibrary library)
    : defaultLanPort = library
          .lookupFunction<ffi.Uint16 Function(), int Function()>(
            'nexdesk_default_lan_port',
          )(),
      _checkId = library
          .lookupFunction<
            ffi.Int32 Function(ffi.Pointer<ffi.Uint8>, ffi.Size),
            _DeviceIdCheck
          >('nexdesk_validate_device_id'),
      _checkLan = library
          .lookupFunction<
            ffi.Int32 Function(
              ffi.Pointer<ffi.Uint8>,
              ffi.Size,
              ffi.Pointer<ffi.Uint8>,
              ffi.Size,
            ),
            _LanCheck
          >('nexdesk_validate_lan_endpoint');

  static const _abiVersion = 1;
  static const _maxTextBytes = 128;

  static RustTargetValidator? tryLoad() {
    final path = locateCoreLibrary();
    if (path == null) return null;

    try {
      final library = ffi.DynamicLibrary.open(path);
      final version = library
          .lookupFunction<ffi.Uint32 Function(), int Function()>(
            'nexdesk_abi_version',
          )();
      return version == _abiVersion ? RustTargetValidator._(library) : null;
    } on ArgumentError {
      return null;
    }
  }

  @override
  final int defaultLanPort;
  final _DeviceIdCheck _checkId;
  final _LanCheck _checkLan;

  @override
  TargetIssue? checkDeviceId(String id) {
    final bytes = utf8.encode(id);
    if (bytes.length > _maxTextBytes) return TargetIssue.text;
    return using((arena) {
      return _issue(_checkId(_allocate(arena, bytes), bytes.length));
    });
  }

  @override
  TargetIssue? checkLan(String address, String port) {
    final addressBytes = utf8.encode(address);
    final portBytes = utf8.encode(port);
    if (addressBytes.length > _maxTextBytes ||
        portBytes.length > _maxTextBytes) {
      return TargetIssue.text;
    }
    return using((arena) {
      return _issue(
        _checkLan(
          _allocate(arena, addressBytes),
          addressBytes.length,
          _allocate(arena, portBytes),
          portBytes.length,
        ),
      );
    });
  }

  static ffi.Pointer<ffi.Uint8> _allocate(Arena arena, List<int> bytes) {
    if (bytes.isEmpty) return ffi.nullptr;
    final buffer = arena<ffi.Uint8>(bytes.length);
    buffer.asTypedList(bytes.length).setAll(0, bytes);
    return buffer;
  }

  static TargetIssue? _issue(int status) => switch (status) {
    0 => null,
    1 => TargetIssue.deviceId,
    2 => TargetIssue.address,
    3 => TargetIssue.port,
    4 => TargetIssue.text,
    _ => TargetIssue.nativeFailure,
  };
}
