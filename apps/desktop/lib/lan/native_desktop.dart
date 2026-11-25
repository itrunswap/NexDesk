import 'dart:convert';
import 'dart:ffi' as ffi;
import 'dart:isolate';

import 'package:ffi/ffi.dart';

import '../preview/native_preview.dart';
import 'connection_password.dart';
import 'native_collaboration.dart';

typedef _Connect = int Function(
  ffi.Pointer<ffi.Uint8>,
  int,
  int,
  ffi.Pointer<ffi.Uint8>,
  int,
  int,
);
typedef _Action = bool Function(
  int,
  int,
  int,
  int,
  int,
  int,
  bool,
  ffi.Pointer<ffi.Uint8>,
  int,
);
typedef _Read = int Function(int, ffi.Pointer<ffi.Uint8>, int);
typedef _DirectoryConnect = int Function(ffi.Pointer<ffi.Uint8>, int);
typedef _Local = int Function(
  ffi.Pointer<ffi.Uint8>,
  int,
  ffi.Pointer<ffi.Uint8>,
  int,
);

enum DesktopAction {
  move(1),
  button(2),
  wheel(3),
  key(4),
  release(5),
  select(6);

  const DesktopAction(this.code);
  final int code;
}

final class NativeDesktop {
  NativeDesktop._(this.preview, ffi.DynamicLibrary library)
    : collaboration = NativeCollaboration.tryLoad(library),
      _directory = _loadDirectory(library),
      _connect = library
          .lookupFunction<
            ffi.Uint64 Function(
              ffi.Pointer<ffi.Uint8>,
              ffi.Size,
              ffi.Uint16,
              ffi.Pointer<ffi.Uint8>,
              ffi.Size,
              ffi.Uint32,
            ),
            _Connect
          >('nexdesk_m2_connect'),
      _trust = library
          .lookupFunction<ffi.Bool Function(ffi.Uint64), bool Function(int)>(
            'nexdesk_m2_trust',
          ),
      _action = library
          .lookupFunction<
            ffi.Bool Function(
              ffi.Uint64,
              ffi.Int32,
              ffi.Uint64,
              ffi.Int32,
              ffi.Int32,
              ffi.Uint32,
              ffi.Bool,
              ffi.Pointer<ffi.Uint8>,
              ffi.Size,
            ),
            _Action
          >('nexdesk_m2_action'),
      _info = library
          .lookupFunction<
            ffi.Size Function(ffi.Uint64, ffi.Pointer<ffi.Uint8>, ffi.Size),
            _Read
          >('nexdesk_m2_info');

  final NativePreview preview;
  final NativeCollaboration? collaboration;
  final _DirectoryConnect? _directory;
  final _Connect _connect;
  final bool Function(int) _trust;
  final _Action _action;
  final _Read _info;

  bool get supportsDirectory => _directory != null;

  static _DirectoryConnect? _loadDirectory(ffi.DynamicLibrary library) {
    try {
      return library.lookupFunction<
        ffi.Uint64 Function(ffi.Pointer<ffi.Uint8>, ffi.Size),
        _DirectoryConnect
      >('nexdesk_m4_connect');
    } on ArgumentError {
      return null;
    }
  }

  static NativeDesktop? tryLoad(NativePreview? preview) {
    if (preview == null) return null;
    try {
      return NativeDesktop._(
        preview,
        ffi.DynamicLibrary.open(preview.libraryPath),
      );
    } on ArgumentError {
      return null;
    }
  }

  int connectTarget(
    String address,
    int port,
    String? origin,
    ConnectionPassword password,
  ) => origin == null
      ? connect(
          address,
          port,
          password.code,
          control: password.control,
          remember: password.remember,
          stored: password.stored,
          capabilities: password.capabilities,
          deviceAuthorization: password.deviceAuthorization,
        )
      : connectDirectory(
          origin,
          address,
          password.code,
          control: password.control,
          remember: password.remember,
          stored: password.stored,
          capabilities: password.capabilities,
          deviceAuthorization: password.deviceAuthorization,
        );

  int connectDirectory(
    String origin,
    String device,
    String code, {
    required bool control,
    required bool remember,
    required bool stored,
    required int capabilities,
    bool deviceAuthorization = false,
  }) {
    final connect = _directory;
    if (connect == null || (!stored && code.length != 6)) return 0;
    final data = utf8.encode(
      jsonEncode({
        'origin': origin,
        'device': device,
        'code': code,
        'flags':
            (control ? 1 : 0) |
            (remember ? 2 : 0) |
            (stored ? 4 : 0) |
            (deviceAuthorization ? 8 : 0),
        'capabilities': capabilities,
      }),
    );
    try {
      return using((arena) {
        final payload = _bytes(arena, data);
        try {
          return connect(payload, data.length);
        } finally {
          payload.asTypedList(data.length).fillRange(0, data.length, 0);
        }
      });
    } finally {
      data.fillRange(0, data.length, 0);
    }
  }

  int connect(
    String address,
    int port,
    String code, {
    required bool control,
    required bool remember,
    required bool stored,
    int? capabilities,
    bool deviceAuthorization = false,
  }) {
    final endpoint = utf8.encode(address);
    final secret = utf8.encode(code);
    try {
      if (endpoint.length > 128 ||
          port < 1 ||
          port > 65535 ||
          (!stored && secret.length != 6)) {
        return 0;
      }
      return using((arena) {
        final temporary = _bytes(arena, secret);
        try {
          final flags =
              (control ? 1 : 0) |
              (remember ? 2 : 0) |
              (stored ? 4 : 0) |
              (deviceAuthorization ? 8 : 0);
          if (capabilities != null && collaboration != null) {
            return collaboration!.connect(
              _bytes(arena, endpoint),
              endpoint.length,
              port,
              temporary,
              secret.length,
              flags,
              capabilities,
            );
          }
          return _connect(
            _bytes(arena, endpoint),
            endpoint.length,
            port,
            temporary,
            secret.length,
            flags,
          );
        } finally {
          if (secret.isNotEmpty) {
            temporary.asTypedList(secret.length).fillRange(0, secret.length, 0);
          }
        }
      });
    } finally {
      secret.fillRange(0, secret.length, 0);
    }
  }

  bool trust(int viewer) => _trust(viewer);

  bool send(
    int viewer,
    DesktopAction action, {
    int generation = 0,
    int x = 0,
    int y = 0,
    int code = 0,
    bool down = false,
    String display = '',
  }) {
    if (display.isEmpty) {
      return _action(
        viewer,
        action.code,
        generation,
        x,
        y,
        code,
        down,
        ffi.nullptr,
        0,
      );
    }
    final bytes = utf8.encode(display);
    return using(
      (arena) => _action(
        viewer,
        action.code,
        generation,
        x,
        y,
        code,
        down,
        _bytes(arena, bytes),
        bytes.length,
      ),
    );
  }

  DesktopInfo inspect(int viewer) => using((arena) {
    const capacity = 8192;
    final output = arena<ffi.Uint8>(capacity);
    final count = _info(viewer, output, capacity);
    return count == 0
        ? const DesktopInfo()
        : DesktopInfo.fromJson(_decode(output, count));
  });

  Future<Object?> local(Map<String, Object?> request) {
    final path = preview.libraryPath;
    return Isolate.run(() => _localOperation(path, request));
  }
}

Object? _localOperation(String path, Map<String, Object?> request) {
  final exchange = ffi.DynamicLibrary.open(path)
      .lookupFunction<
        ffi.Size Function(
          ffi.Pointer<ffi.Uint8>,
          ffi.Size,
          ffi.Pointer<ffi.Uint8>,
          ffi.Size,
        ),
        _Local
      >('nexdesk_m2_local');
  final bytes = utf8.encode(jsonEncode(request));
  return using((arena) {
    const capacity = 32768;
    final result = arena<ffi.Uint8>(capacity);
    final count = exchange(
      _bytes(arena, bytes),
      bytes.length,
      result,
      capacity,
    );
    if (count == 0) throw StateError('本机设置请求无效。');
    final reply = _decode(result, count);
    if (reply.containsKey('error')) throw StateError('无法访问本机设置或系统凭据。');
    return reply['ok'];
  });
}

ffi.Pointer<ffi.Uint8> _bytes(Arena arena, List<int> bytes) {
  if (bytes.isEmpty) return ffi.nullptr;
  final data = arena<ffi.Uint8>(bytes.length);
  data.asTypedList(bytes.length).setAll(0, bytes);
  return data;
}

Map<String, Object?> _decode(ffi.Pointer<ffi.Uint8> bytes, int length) =>
    (jsonDecode(utf8.decode(bytes.asTypedList(length))) as Map)
        .cast<String, Object?>();

final class DesktopInfo {
  const DesktopInfo({
    this.identity,
    this.generation = 0,
    this.canInput = false,
    this.selected = '',
    this.displays = const [],
    this.notice = '',
    this.storageFailed = false,
  });

  factory DesktopInfo.fromJson(Map<String, Object?> json) {
    final data = json['desktop'] as Map? ?? const {};
    final displays = data['displays'] as List? ?? const [];
    return DesktopInfo(
      identity: json['identity'] as String?,
      generation: data['generation'] as int? ?? 0,
      canInput: data['can_input'] == true,
      selected: data['selected'] as String? ?? '',
      displays: displays
          .whereType<Map>()
          .map(
            (display) => DisplayChoice(
              display['id'] as String,
              display['label'] as String,
            ),
          )
          .toList(growable: false),
      notice: data['notice'] as String? ?? '',
      storageFailed: json['storage_failed'] == true,
    );
  }

  final String? identity;
  final int generation;
  final bool canInput, storageFailed;
  final String selected, notice;
  final List<DisplayChoice> displays;
}

final class DisplayChoice {
  const DisplayChoice(this.id, this.label);
  final String id, label;
}

final class RecentDevice {
  const RecentDevice({
    required this.endpoint,
    required this.address,
    required this.port,
    required this.label,
    required this.remembered,
    this.origin,
  });
  factory RecentDevice.fromJson(Map json) => RecentDevice(
    endpoint: json['endpoint'] as String,
    address: json['address'] as String,
    port: json['port'] as int,
    label: json['label'] as String,
    remembered: json['remembered'] == true,
  );
  factory RecentDevice.fromDirectory(Map json) => RecentDevice(
    endpoint: "${json['device']} · ${json['origin']}",
    address: json['device'] as String,
    port: 0,
    label: json['label'] as String,
    remembered: json['remembered'] == true,
    origin: json['origin'] as String,
  );
  final String endpoint, address, label;
  final String? origin;
  final int port;
  final bool remembered;
}
